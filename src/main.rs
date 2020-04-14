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
        // Documentation isn't super clear, but it's some type of struct
        // which is an iterator
        // source: https://doc.rust-lang.org/stable/std/env/struct.Args.html
        let user = parts.next().unwrap().to_owned();
        // we look at the next itme
        match parts.next() {
            // if there is something, it will be the repo
            // Series is an enum, can either be User(string), or Repo(string, string)
            // and so we push in a struct, which looks like a tuple, first the user, then the repo
            // .to_owned()
            // Creates owned data from borrowed data, usually by cloning
            // source: https://doc.rust-lang.org/std/borrow/trait.ToOwned.html
            Some(repo) => args.push(Series::Repo(user, repo.to_owned())),
            // if not, it means we only have the user
            // other struct is used, we create a Series::User struct and pass in the user 
            None => args.push(Series::User(user)),
        }
    }

    // we need a github token to add to our request
    let authorization = match env::var("GITHUB_TOKEN") {
        // it's a bearer token, we trim any whitespace 
        // trims both sides
        // "Returns a string slice with leading and trailing whitespace removed."
        // source: https://doc.rust-lang.org/std/string/struct.String.html#method.trim
        Ok(token) => format!("bearer {}", token.trim()),
        // If it's not there, we tell the user we're missing it
        Err(_) => {
            // eprint! is like println! but prints to the standard error 
            // like console.error in js vs. console.log
            // TIL: "Use eprint! only for error and progress messages. Use print! instead for the primary output of your program."
            // Source: https://doc.rust-lang.org/std/macro.eprint.html
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

    // Vec is a "growable array type"
    // and we don't know how big the "work" will be
    // "work" is essentially all the data points relate to the star history
    // @question wait...is it though? Will have to come back here
    // Source: https://doc.rust-lang.org/std/vec/struct.Vec.html
    let mut work = Vec::new();
    // couldn't find much ih the docs related to Map. 
    // Source: https://doc.rust-lang.org/beta/core/iter/struct.Map.html
    let mut stars = Map::new();
    // this is series because remember above we created Series::User or Series::Repo and 
    // pushed those into args
    for series in &args {
        // I _think_ insert is inserting a key-value into stars, which is a Map
        // so we clone the series (remember this is Series::User, or Series::Repo)
        // and that's the key, and then the value is a new Set.
        // @question why can I only find docs about HashSets but not Sets in Rust?
        // source: https://doc.rust-lang.org/std/collections/struct.HashSet.html 
        // I assume it's similar to a set in JS which is a collection of unique values
        // source: https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Set
        stars.insert(series.clone(), Set::new());
        // @question what is Work? It looks like a struct, but would be nice to have a description or something
        // and cursor...I saw that in the gql request, but wasn't sure what it was
        // Work is a struct with two properties: series and cursor
        // series is what we've seen so far (Series::User/Repo)
        // cursor: when we start the work, we don't know if it will all come back in one request
        // hence why cursor is None -> null in JS
        // which we initialize with None. I think because Option can be Result or None
        // We take this new struct (Work) and push it into our work Vec
        work.push(Work {
            series: series.clone(),
            cursor: Cursor(None),
        });
    }

    // Create a new Client, so we can make requests to the GitHub API
    // (note to self: I guess I took that for granted and thought you could "just do it" without any packages)
    let client = Client::new();
    // "Stderr, also known as standard error, is the default file descriptor where a process can write error messages"
    // source: https://www.computerhope.com/jargon/s/stderr.htm
    let mut stderr = std::io::stderr();
    // While we have work to do (i.e. while work is not empty)
    while !work.is_empty() {
        // stopped here
        // We want to batch the work, we compare the length of the work and the number 50 
        // and we take the smaller size and make that our batch_size
        // @question is this because the project supports multiple users/projects at once?
        let batch_size = cmp::min(work.len(), 50);
        // @question why are we splitting off at the batch_size? Both why in general, and why batch_size for the split point?
        // source: https://doc.rust-lang.org/std/vec/struct.Vec.html#method.split_off
        // and that's what we defer, which I assume we'll do after we finish the first batch
        let defer = work.split_off(batch_size);
        // according to the std lib docs
        // "Moves src into the referenced dest, returning the previous dest value.
        // Neither value is dropped."
        // source: https://doc.rust-lang.org/std/mem/fn.replace.html
        // @question why do we need to replace work with defer???
        let batch = mem::replace(&mut work, defer);

        // This is to create our GraphQL query to the GitHub API
        let mut query = String::new();
        query += "{\n";
        // iterate over the batch
        for (i, work) in batch.iter().enumerate() {
            // @question what is cursor? 
            let cursor = &work.cursor;
            // Here, we check the work.series to see what we have
            // then use our helper fn's "query_user" and "query_repo"
            // which return our user and repo queries respectively
            query += &match &work.series {
                Series::User(user) => query_user(i, user, cursor),
                Series::Repo(user, repo) => query_repo(i, user, repo, cursor),
            };
        }
        // @question it would be really helpful to have some examples
        // i.e. when you ask for start history of user dtolnay, the query looks like this
        // i.e. when you ask for two users dtolnay and jsjoeio, the query looks like that
        query += "}\n";

        // Send our GraphQL request and get back json
        let json = client
            .post("https://api.github.com/graphql")
            .header(USER_AGENT, "dtolnay/star-history")
            .header(AUTHORIZATION, &authorization)
            .json(&Request { query })
            .send()?
            .text()?;

        // we grab the response from the json
        // @question what is serde_json?
        let response: Response = serde_json::from_str(&json).map_err(Error::DecodeResponse)?;
        // if there is some message, we show that as the error message
        // @question what would be an example of that? I don't understand the difference between a message and an error.message?
        if let Some(message) = response.message {
            return Err(Error::GitHub(message));
        }
        // if there is an error, we return the github error
        if let Some(error) = response.errors.into_iter().next() {
            return Err(Error::GitHub(error.message));
        }

        // Otherwise, we have the data!
        let mut data = response.data;
        // start a queue from the batch?
        let mut queue = batch.into_iter();
        // @question I'm not too sure what this part of the code is doing
        while let Some(node) = data.pop_front() {
            let id = queue.next();
            match node {
                // check for no user data or repo data
                // in which case we would return the proper error
                Data::User(None) | Data::Repo(None) => match id.unwrap().series {
                    Series::User(user) => return Err(Error::NoSuchUser(user)),
                    Series::Repo(user, repo) => return Err(Error::NoSuchRepo(user, repo)),
                },
                // if we have the user data
                Data::User(Some(node)) => {
                    let user = node.login;
                    for repo in node.repositories.nodes {
                        data.push_back(Data::Repo(Some(repo)));
                    }

                    // oh now i get it!
                    // if there is more data
                    // we push to work so we can do this all over again, like a loop
                    // and the cursor is where we left off
                    if node.repositories.page_info.has_next_page {
                        work.push(Work {
                            series: Series::User(user),
                            cursor: node.repositories.page_info.end_cursor,
                        });
                    }
                }
                // the repo gets a little trickier
                Data::Repo(Some(node)) => {
                    let user = node.owner.login;
                    let repo = node.name;

                    // if there are stargazers (which we should see what happens if there is no stargazers, like a new repo)
                    if let Some(stargazers) = node.stargazers {
                        // @question why do we need to clone the user and create a "let series"?
                        // yeah, I don't understand this block here from 506 - 510? What's the purpose?
                        // are we cloning and collecting so that we have a copy of this data, which will then be part of
                        // the large collection used for the graph? that's my best guess
                        let series = Series::User(user.clone());
                        let user_stars = stars.entry(series).or_default();
                        for star in &stargazers.edges {
                            user_stars.insert(star.clone());
                        }

                        // same as aboe. my guess is we want to clone this data and keep track
                        // in case we need to make more requests and get more data
                        let series = Series::Repo(user.clone(), repo.clone());
                        let repo_stars = stars.entry(series).or_default();
                        for star in &stargazers.edges {
                            repo_stars.insert(star.clone());
                        }

                        // Same thing
                        // If there's another page, we need to do this all over again
                        // We then know the end_cursor because it has come back as part of the GraphQL request response payload
                        // @todo - I think the logic goes here for compare ing the 
                        if stargazers.page_info.has_next_page {
                            work.push(Work {
                                series: Series::Repo(user, repo),
                                cursor: stargazers.page_info.end_cursor,
                            });
                        }
                    } else {
                        // @question why the else block? if we have the data, can't we stop?
                        // instead of adding more work?
                        work.push(Work {
                            series: Series::Repo(user, repo),
                            cursor: Cursor(None),
                        });
                    }
                }
            }
        }

        // @question what is this for?
        let _ = write!(stderr, ".");
        let _ = stderr.flush();
    }
    let _ = writeln!(stderr);

    // Grabe the time now
    let now = Utc::now();
    // loop through our stars
    for set in stars.values_mut() {
        if let Some(first) = set.iter().cloned().next() {
            // and insert a star into our set
            // to be used for the graph
            set.insert(Star {
                time: first.time - Duration::seconds(1),
                node: Default::default(),
            });
        }
        // @question what's happening here?
        // @question what is .next_back()?
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

    // @question, what are we building here? What is this data going to be?
    // oh... var data...looks like a JavaScript value that will be used in graph
    // looks like JSON
    // @todo Would be nice to see the example
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

    // We then look at our index.html file and replace our data with our actual data
    let html = include_str!("index.html").replace("var data = [];", &data);
    // create a temp directory 
    let dir = env::temp_dir().join("star-history");
    fs::create_dir_all(&dir)?;
    // used the now timestamp for the name
    let path = dir.join(format!("{}.html", now.timestamp()));
    // write the file to disk
    fs::write(&path, html)?;

    // open the path
    if opener::open(&path).is_err() {
        eprintln!("graph written to {}", path.display());
    }
    // @question what is this Ok(()) for?
    Ok(())
}

// A function to query a GitHub user
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

// A function to query a GitHub repo
fn query_repo(i: usize, user: &str, repo: &str, forward_cursor: &Cursor, backward_cursor: &Cursor) -> String {
    r#"
        repo$i: repository(owner: "$user", name: "$repo") {
          name
          owner {
            login
          }
          forwardStargazers: stargazers(after: $forward_cursor, first: 100, orderBy: { direction: ASC, field: STARRED_AT }) {
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
          backwardStargazers: stargazers(before: $backward_cursor, last: 100, orderBy: { direction: DESC, field: STARRED_AT }) {
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
    .replace("$forward_cursor", &forward_cursor.to_string())
    .replace("$backward_cursor", &backward_cursor.to_string())
}
