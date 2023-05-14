//! [![github]](https://github.com/dtolnay/star-history)&ensp;[![crates-io]](https://crates.io/crates/star-history)&ensp;[![docs-rs]](https://docs.rs/star-history)
//!
//! [github]: https://img.shields.io/badge/github-8da0cb?style=for-the-badge&labelColor=555555&logo=github
//! [crates-io]: https://img.shields.io/badge/crates.io-fc8d62?style=for-the-badge&labelColor=555555&logo=rust
//! [docs-rs]: https://img.shields.io/badge/docs.rs-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs

#![allow(
    clippy::cast_lossless,
    clippy::default_trait_access,
    clippy::let_underscore_untyped,
    // Clippy bug: https://github.com/rust-lang/rust-clippy/issues/7422
    clippy::nonstandard_macro_braces,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::toplevel_ref_arg,
    clippy::uninlined_format_args,
)]

mod log;

use crate::log::Log;
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
use std::io;
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
    gh auth login
    star-history [USER ...] [USER/REPO ...]

EXAMPLES:
    star-history dtolnay
    star-history dtolnay/syn dtolnay/quote
    star-history serde-rs/serde
",
);

static MISSING_TOKEN: &str = "\
Error: GitHub auth token is not set up.

(Expected config file: {{path}})

Run `gh auth login` to store a GitHub login token. The `gh` CLI
can be installed from <https://cli.github.com>.

If you prefer not to use the `gh` CLI, you can instead provide
a token to star-history through the GITHUB_TOKEN environment
variable. Head to <https://github.com/settings/tokens> and click
\"Generate new token (classic)\". The default public access
permission is sufficient -- you can leave all the checkboxes
empty. Save the generated token somewhere like ~/.githubtoken
and use `export GITHUB_TOKEN=$(cat ~/.githubtoken)`.
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
    GhToken(#[from] gh_token::Error),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Eq, Clone)]
enum Series {
    Owner(String),
    Repo(String, String),
}

impl Display for Series {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Series::Owner(owner) => formatter.write_str(owner)?,
            Series::Repo(owner, repo) => {
                formatter.write_str(owner)?;
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
            (Series::Owner(lowner), Series::Owner(rowner)) => {
                lowner.to_lowercase().cmp(&rowner.to_lowercase())
            }
            (Series::Repo(lowner, lrepo), Series::Repo(rowner, rrepo)) => {
                (lowner.to_lowercase(), lrepo.to_lowercase())
                    .cmp(&(rowner.to_lowercase(), rrepo.to_lowercase()))
            }
            (Series::Owner(_), Series::Repo(..)) => Ordering::Less,
            (Series::Repo(..), Series::Owner(_)) => Ordering::Greater,
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

#[derive(Serialize, Deserialize, Debug)]
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

#[derive(Deserialize, Debug)]
struct Response {
    message: Option<String>,
    #[serde(default, deserialize_with = "deserialize_data")]
    data: VecDeque<Data>,
    #[serde(default)]
    errors: Vec<Message>,
}

#[derive(Deserialize, Debug)]
struct Message {
    message: String,
}

#[derive(Debug)]
enum Data {
    Owner(Option<Owner>),
    Repo(Option<Repo>),
}

#[derive(Deserialize, Debug)]
struct Owner {
    login: String,
    repositories: Repositories,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Repositories {
    page_info: PageInfo,
    nodes: Vec<Repo>,
}

#[derive(Deserialize, Debug)]
struct Repo {
    name: String,
    owner: Account,
    stargazers: Option<Stargazers>,
}

#[derive(Deserialize, Ord, PartialOrd, Eq, PartialEq, Clone, Default, Debug)]
struct Account {
    login: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Stargazers {
    page_info: PageInfo,
    #[serde(deserialize_with = "non_nulls")]
    edges: Vec<Star>,
}

#[derive(Deserialize, Ord, PartialOrd, Eq, PartialEq, Clone, Debug)]
struct Star {
    #[serde(rename = "starredAt")]
    time: DateTime<Utc>,
    node: Account,
}

#[derive(Deserialize, Debug)]
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
                if key.starts_with("owner") {
                    let owner = map.next_value::<Option<Owner>>()?;
                    data.push_back(Data::Owner(owner));
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
    let ref mut log = Log::new();
    if let Err(err) = try_main(log) {
        log.error(err);
        process::exit(1);
    }
}

fn try_main(log: &mut Log) -> Result<()> {
    let mut args = Vec::new();
    for arg in env::args().skip(1) {
        if arg == "--help" {
            print!("{}", HELP);
            process::exit(0);
        } else if arg == "--version" {
            println!("{}", VERSION);
            process::exit(0);
        }
        let mut parts = arg.splitn(2, '/');
        let owner = parts.next().unwrap();
        match parts.next() {
            Some(repo) => {
                let owner = owner.to_owned();
                let repo = repo.to_owned();
                args.push(Series::Repo(owner, repo));
            }
            None => {
                let owner = owner.strip_prefix('@').unwrap_or(owner).to_owned();
                args.push(Series::Owner(owner));
            }
        }
    }

    let github_token = match gh_token::get() {
        Ok(token) => token,
        Err(gh_token::Error::NotConfigured(path)) => {
            let path_lossy = path.to_string_lossy();
            let message = MISSING_TOKEN.replace("{{path}}", &path_lossy);
            eprint!("{}", message);
            process::exit(1);
        }
        Err(error) => return Err(Error::GhToken(error)),
    };
    let authorization = format!("bearer {}", github_token.trim());

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
    while !work.is_empty() {
        let batch_size = cmp::min(work.len(), 50);
        let defer = work.split_off(batch_size);
        let batch = mem::replace(&mut work, defer);

        let mut query = String::new();
        query += "{\n";
        for (i, work) in batch.iter().enumerate() {
            let cursor = &work.cursor;
            query += &match &work.series {
                Series::Owner(owner) => query_owner(i, owner, cursor),
                Series::Repo(owner, repo) => query_repo(i, owner, repo, cursor),
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
        for err in response.errors {
            log.error(Error::GitHub(err.message));
        }

        let mut data = response.data;
        let mut queue = batch.into_iter();
        while let Some(node) = data.pop_front() {
            let id = queue.next();
            match node {
                Data::Owner(None) | Data::Repo(None) => match id.unwrap().series {
                    Series::Owner(owner) => return Err(Error::NoSuchUser(owner)),
                    Series::Repo(owner, repo) => return Err(Error::NoSuchRepo(owner, repo)),
                },
                Data::Owner(Some(node)) => {
                    let owner = node.login;
                    for repo in node.repositories.nodes {
                        data.push_back(Data::Repo(Some(repo)));
                    }

                    if node.repositories.page_info.has_next_page {
                        work.push(Work {
                            series: Series::Owner(owner),
                            cursor: node.repositories.page_info.end_cursor,
                        });
                    }
                }
                Data::Repo(Some(node)) => {
                    let owner = node.owner.login;
                    let repo = node.name;

                    if let Some(stargazers) = node.stargazers {
                        let series = Series::Owner(owner.clone());
                        let owner_stars = stars.entry(series).or_default();
                        for star in &stargazers.edges {
                            owner_stars.insert(star.clone());
                        }

                        let series = Series::Repo(owner.clone(), repo.clone());
                        let repo_stars = stars.entry(series).or_default();
                        for star in &stargazers.edges {
                            repo_stars.insert(star.clone());
                        }

                        if stargazers.page_info.has_next_page {
                            work.push(Work {
                                series: Series::Repo(owner, repo),
                                cursor: stargazers.page_info.end_cursor,
                            });
                        }
                    } else {
                        work.push(Work {
                            series: Series::Repo(owner, repo),
                            cursor: Cursor(None),
                        });
                    }
                }
            }
        }

        log.tick();
    }

    let now = Utc::now();
    for set in stars.values_mut() {
        if let Some(first) = set.iter().next() {
            let first_time = first.time;
            set.insert(Star {
                time: first_time - Duration::seconds(1),
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
    let path = dir.join(format!("{}.html", now.timestamp_millis()));
    fs::write(&path, html)?;

    if opener::open(&path).is_err() {
        writeln!(log, "graph written to {}", path.display());
    }
    Ok(())
}

fn query_owner(i: usize, login: &str, cursor: &Cursor) -> String {
    r#"
        owner$i: repositoryOwner(login: "$login") {
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
    .replace("$login", login)
    .replace("$cursor", &cursor.to_string())
}

fn query_repo(i: usize, owner: &str, repo: &str, cursor: &Cursor) -> String {
    r#"
        repo$i: repository(owner: "$owner", name: "$repo") {
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
    .replace("$owner", owner)
    .replace("$repo", repo)
    .replace("$cursor", &cursor.to_string())
}
