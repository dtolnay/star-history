use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde::de::{Error, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap as Map, BTreeSet as Set, VecDeque};
use std::env;
use std::fmt::{self, Display};
use std::fs;
use std::io::Write;
use std::mem;
use std::process;

static HELP: &str = concat!(
    "star-history ",
    env!("CARGO_PKG_VERSION"),
    "
David Tolnay <dtolnay@gmail.com>

Produce a graph showing number of GitHub stars of a user or repo over time.

USAGE:
    star-history [USER ...] [USER/REPO ...]

EXAMPLES:
    export GITHUB_TOKEN=$(cat ~/.githubtoken)
    star-history dtolnay
    star-history dtolnay/syn dtolnay/quote
    star-history serde-rs/serde
",
);

static MISSING_TOKEN: &str = "\
Error: environment variable GITHUB_TOKEN must be defined.

Log in to https://github.com/settings/tokens and click \"Generate new
token\". The default public access permission is sufficient -- you can
leave all the checkboxes empty. Save the generated token somewhere like 
~/.githubtoken and use:

    export GITHUB_TOKEN=$(cat ~/.githubtoken)

";

#[derive(Eq, Clone)]
enum Series {
    User(String),
    Repo(String, String),
}

impl Display for Series {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Series::User(user) => formatter.write_str(user)?,
            Series::Repo(user, repo) => {
                formatter.write_str(user)?;
                formatter.write_str("/")?;
                formatter.write_str(repo)?;
            }
        }
        Ok(())
    }
}

impl Ord for Series {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Series::User(luser), Series::User(ruser)) => {
                luser.to_lowercase().cmp(&ruser.to_lowercase())
            }
            (Series::Repo(luser, lrepo), Series::Repo(ruser, rrepo)) => {
                (luser.to_lowercase(), lrepo.to_lowercase())
                    .cmp(&(ruser.to_lowercase(), rrepo.to_lowercase()))
            }
            (Series::User(_), Series::Repo(..)) => Ordering::Less,
            (Series::Repo(..), Series::User(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for Series {
    fn partial_cmp(&self, other: &Series) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Series {
    fn eq(&self, other: &Series) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct Cursor(Option<String>);

impl Display for Cursor {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match &self.0 {
            Some(cursor) => {
                formatter.write_str("\"")?;
                formatter.write_str(cursor)?;
                formatter.write_str("\"")?;
            }
            None => formatter.write_str("null")?,
        }
        Ok(())
    }
}

struct Work {
    series: Series,
    cursor: Cursor,
}

#[derive(Serialize)]
struct Request {
    query: String,
}

#[derive(Deserialize)]
struct Response {
    message: Option<String>,
    #[serde(default, deserialize_with = "deserialize_data")]
    data: VecDeque<Data>,
    #[serde(default)]
    errors: Vec<Message>,
}

#[derive(Deserialize)]
struct Message {
    message: String,
}

enum Data {
    User(Option<User>),
    Repo(Option<Repo>),
}

#[derive(Deserialize)]
struct User {
    login: String,
    repositories: Repositories,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repositories {
    page_info: PageInfo,
    nodes: Vec<Repo>,
}

#[derive(Deserialize)]
struct Repo {
    name: String,
    owner: Account,
    stargazers: Option<Stargazers>,
}

#[derive(Deserialize, Ord, PartialOrd, Eq, PartialEq, Clone, Default)]
struct Account {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stargazers {
    page_info: PageInfo,
    edges: Vec<Star>,
}

#[derive(Deserialize, Ord, PartialOrd, Eq, PartialEq, Clone)]
struct Star {
    #[serde(rename = "starredAt")]
    time: DateTime<Utc>,
    node: Account,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Cursor,
}

fn deserialize_data<'de, D>(deserializer: D) -> Result<VecDeque<Data>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ResponseVisitor;

    impl<'de> Visitor<'de> for ResponseVisitor {
        type Value = VecDeque<Data>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("Map<String, Data>")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut data = VecDeque::new();
            while let Some(key) = map.next_key::<String>()? {
                if key.starts_with("user") {
                    let user = map.next_value::<Option<User>>()?;
                    data.push_back(Data::User(user));
                } else if key.starts_with("repo") {
                    let repo = map.next_value::<Option<Repo>>()?;
                    data.push_back(Data::Repo(repo));
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(data)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: Error,
        {
            Ok(VecDeque::new())
        }
    }

    deserializer.deserialize_any(ResponseVisitor)
}

fn main() -> Result<()> {
    let mut args = Vec::new();
    for arg in env::args().skip(1) {
        if arg == "--help" {
            print!("{}", HELP);
            process::exit(0);
        }
        let mut parts = arg.splitn(2, '/');
        let user = parts.next().unwrap().to_owned();
        match parts.next() {
            Some(repo) => args.push(Series::Repo(user, repo.to_owned())),
            None => args.push(Series::User(user)),
        }
    }

    let authorization = match env::var("GITHUB_TOKEN") {
        Ok(token) => format!("bearer {}", token.trim()),
        Err(_) => {
            eprint!("{}", MISSING_TOKEN);
            process::exit(1);
        }
    };

    if args.is_empty() {
        eprint!("{}", HELP);
        process::exit(1);
    }

    let mut work = Vec::new();
    let mut stars = Map::new();
    for series in &args {
        stars.insert(series.clone(), Set::new());
        work.push(Work {
            series: series.clone(),
            cursor: Cursor(None),
        });
    }

    let client = Client::new();
    let mut stderr = std::io::stderr();
    while !work.is_empty() {
        let mut query = String::new();
        query += "{\n";
        for (i, work) in work.iter().enumerate() {
            let cursor = &work.cursor;
            query += &match &work.series {
                Series::User(user) => query_user(i, user, cursor),
                Series::Repo(user, repo) => query_repo(i, user, repo, cursor),
            };
        }
        query += "}\n";

        let json = client
            .post("https://api.github.com/graphql")
            .header(USER_AGENT, "dtolnay/star-history")
            .header(AUTHORIZATION, &authorization)
            .json(&Request { query })
            .send()?
            .text()?;

        let response: Response =
            serde_json::from_str(&json).context("error decoding response body")?;
        if let Some(message) = response.message {
            bail!("error from GitHub api: {}", message);
        }
        if let Some(error) = response.errors.first() {
            bail!("error from GitHub api: {}", error.message);
        }

        let mut data = response.data;
        let mut queue = mem::take(&mut work).into_iter();
        while let Some(node) = data.pop_front() {
            let id = queue.next();
            match node {
                Data::User(None) | Data::Repo(None) => match id.unwrap().series {
                    Series::User(user) => bail!("no such user: {}", user),
                    Series::Repo(user, repo) => bail!("no such repository: {}/{}", user, repo),
                },
                Data::User(Some(node)) => {
                    let user = node.login;
                    for repo in node.repositories.nodes {
                        data.push_back(Data::Repo(Some(repo)));
                    }

                    if node.repositories.page_info.has_next_page {
                        work.push(Work {
                            series: Series::User(user),
                            cursor: node.repositories.page_info.end_cursor,
                        });
                    }
                }
                Data::Repo(Some(node)) => {
                    let user = node.owner.login;
                    let repo = node.name;

                    if let Some(stargazers) = node.stargazers {
                        let series = Series::User(user.clone());
                        let user_stars = stars.entry(series).or_default();
                        for star in &stargazers.edges {
                            user_stars.insert(star.clone());
                        }

                        let series = Series::Repo(user.clone(), repo.clone());
                        let repo_stars = stars.entry(series).or_default();
                        for star in &stargazers.edges {
                            repo_stars.insert(star.clone());
                        }

                        if stargazers.page_info.has_next_page {
                            work.push(Work {
                                series: Series::Repo(user, repo),
                                cursor: stargazers.page_info.end_cursor,
                            });
                        }
                    } else {
                        work.push(Work {
                            series: Series::Repo(user, repo),
                            cursor: Cursor(None),
                        });
                    }
                }
            }
        }

        let _ = write!(stderr, ".");
        let _ = stderr.flush();
    }
    let _ = writeln!(stderr);

    let now = Utc::now();
    for set in stars.values_mut() {
        if let Some(first) = set.iter().cloned().next() {
            set.insert(Star {
                time: first.time - Duration::seconds(1),
                node: Default::default(),
            });
        }
        match set.iter().next_back() {
            Some(last) if last.time >= now => {}
            _ => {
                set.insert(Star {
                    time: now,
                    node: Default::default(),
                });
            }
        }
    }

    let mut data = String::new();
    data += "var data = [\n";
    for arg in &args {
        data += "      {\"name\":\"";
        data += &arg.to_string();
        data += "\", \"values\":[\n";
        let stars = &stars[arg];
        for (i, star) in stars.iter().enumerate() {
            data += "        {\"time\":";
            data += &star.time.timestamp().to_string();
            data += ", \"stars\":";
            data += &(i - (star.time == now) as usize).to_string();
            data += "},\n";
        }
        data += "      ]},\n";
    }
    data += "    ];";

    let html = include_str!("index.html").replace("var data = [];", &data);
    let dir = env::temp_dir().join("star-history");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.html", now.timestamp()));
    fs::write(&path, html)?;

    if opener::open(&path).is_err() {
        eprintln!("graph written to {}", path.display());
    }
    Ok(())
}

fn query_user(i: usize, user: &str, cursor: &Cursor) -> String {
    r#"
        user$i: user(login: "$user") {
          login
          repositories(after: $cursor, first: 100, isFork: false, privacy: PUBLIC, ownerAffiliations: [OWNER]) {
            pageInfo {
              hasNextPage
              endCursor
            }
            nodes {
              name
              owner {
                login
              }
            }
          }
        }
    "#
    .replace("$i", &i.to_string())
    .replace("$user", user)
    .replace("$cursor", &cursor.to_string())
}

fn query_repo(i: usize, user: &str, repo: &str, cursor: &Cursor) -> String {
    r#"
        repo$i: repository(owner: "$user", name: "$repo") {
          name
          owner {
            login
          }
          stargazers(after: $cursor, first: 100) {
            pageInfo {
              hasNextPage
              endCursor
            }
            edges {
              node {
                login
              }
              starredAt
            }
          }
        }
    "#
    .replace("$i", &i.to_string())
    .replace("$user", user)
    .replace("$repo", repo)
    .replace("$cursor", &cursor.to_string())
}
