use anyhow::anyhow;
use chrono::{DateTime, Duration, Utc};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::cmp::{self, Ordering};
use std::collections::{BTreeMap as Map, BTreeSet as Set, VecDeque};
use std::env;
use std::fmt::{self, Display};
use std::fs;
use std::io::{self, Write};
use std::marker::PhantomData;
use std::mem;
use std::process;
use thiserror::Error;

static VERSION: &str = concat!("star-history ", env!("CARGO_PKG_VERSION"));

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

#[derive(Error, Debug)]
enum Error {
    #[error("Error from GitHub api: {0}")]
    GitHub(String),
    #[error("failed to decode response body")]
    DecodeResponse(#[source] serde_json::Error),
    #[error("no such user: {0}")]
    NoSuchUser(String),
    #[error("no such repository: {0}/{1}")]
    NoSuchRepo(String, String),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

type Result<T, E = Error> = std::result::Result<T, E>;

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
    #[serde(deserialize_with = "non_nulls")]
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
            E: de::Error,
        {
            Ok(VecDeque::new())
        }
    }

    deserializer.deserialize_any(ResponseVisitor)
}

fn non_nulls<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct NonNullsVisitor<T>(PhantomData<fn() -> T>);

    impl<'de, T> Visitor<'de> for NonNullsVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("array")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut vec = Vec::new();
            while let Some(next) = seq.next_element::<Option<T>>()? {
                vec.extend(next);
            }
            Ok(vec)
        }
    }

    let visitor = NonNullsVisitor(PhantomData);
    deserializer.deserialize_seq(visitor)
}

fn main() {
    // Check for GitHub error when calling try_main
    if let Err(err) = try_main() {
        let prefix = match err {
            Error::GitHub(_) => "", // already starts with "Error"
            _ => "Error: ",
        };
        // If there is one, we write it out to the user
        let _ = writeln!(io::stderr(), "{}{:?}", prefix, anyhow!(err));
        // Then exit
        process::exit(1);
    }
}

// This is where the main program actually happens
fn try_main() -> Result<()> {
    // Create a mutable args to hold our cli args
    let mut args = Vec::new();
    for arg in env::args().skip(1) {
        // First check for the help flag
        if arg == "--help" {
            print!("{}", HELP);
            // Exit with 0 because --help is a valid argument
            process::exit(0);
        } else if arg == "--version" {
            println!("{}", VERSION);
            // Exit with 0 because --version is also a valid argument
            process::exit(0);
        }
        // Not sure what this does, let's test in Rust playground...
        // Update: https://play.rust-lang.org/?version=stable&mode=debug&edition=2018&gist=f11e6b822f3e0332fdc2256f1ebb9488
        // We split on the /
        let mut parts = arg.splitn(2, '/');
        // the user will be the first argument in this structure thing
        // @question what is this structure thing? Is it a tuple, an array, a vec, something else? 
        let user = parts.next().unwrap().to_owned();
        // we look at the next itme
        match parts.next() {
            // if there is something, it will be the repo
            // @question what is a Series?
            Some(repo) => args.push(Series::Repo(user, repo.to_owned())),
            // if not, it means we only have the user
            None => args.push(Series::User(user)),
        }
    }

    // we need a github token to add to our request
    let authorization = match env::var("GITHUB_TOKEN") {
        // it's a bearer token, we trim any whitespace 
        // @question (note: does trim only trim end, or both sides?)
        Ok(token) => format!("bearer {}", token.trim()),
        // If it's not there, we tell the user we're missing it
        Err(_) => {
            // @question what's the difference between eprint! and println!? console.error vs console.log?
            eprint!("{}", MISSING_TOKEN);
            // and exit with 1, because it's unexpected
            process::exit(1);
        }
    };

    // if no args, we also print help
    if args.is_empty() {
        eprint!("{}", HELP);
        // and exit with 1, because it's unexpected
        process::exit(1);
    }

    // @question why are we using a Vec, and why is it called work, what exactly is work?
    let mut work = Vec::new();
    // @question why are we using a Map? (haven't looked at Maps in Rust yet)
    let mut stars = Map::new();
    // @question what is series?
    for series in &args {
        // @confused hmm... what does .insert do on a Map, insert into the Map these two things as a tuple?
        // but what are clone the args
        stars.insert(series.clone(), Set::new());
        // @question what is Work? It looks like a struct, but would be nice to have a description or something
        // and cursor...I saw that in the gql request, but wasn't sure what it was
        work.push(Work {
            series: series.clone(),
            cursor: Cursor(None),
        });
    }
    // Stopped here...

    let client = Client::new();
    let mut stderr = std::io::stderr();
    while !work.is_empty() {
        let batch_size = cmp::min(work.len(), 50);
        let defer = work.split_off(batch_size);
        let batch = mem::replace(&mut work, defer);

        let mut query = String::new();
        query += "{\n";
        for (i, work) in batch.iter().enumerate() {
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

        let response: Response = serde_json::from_str(&json).map_err(Error::DecodeResponse)?;
        if let Some(message) = response.message {
            return Err(Error::GitHub(message));
        }
        if let Some(error) = response.errors.into_iter().next() {
            return Err(Error::GitHub(error.message));
        }

        let mut data = response.data;
        let mut queue = batch.into_iter();
        while let Some(node) = data.pop_front() {
            let id = queue.next();
            match node {
                Data::User(None) | Data::Repo(None) => match id.unwrap().series {
                    Series::User(user) => return Err(Error::NoSuchUser(user)),
                    Series::Repo(user, repo) => return Err(Error::NoSuchRepo(user, repo)),
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
            data += &(i.saturating_sub((star.time == now) as usize)).to_string();
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
