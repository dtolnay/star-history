GitHub star history
===================

[<img alt="github" src="https://img.shields.io/badge/github-dtolnay/star--history-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20">](https://github.com/dtolnay/star-history)
[<img alt="crates.io" src="https://img.shields.io/crates/v/star-history.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/star-history)
[<img alt="build status" src="https://img.shields.io/github/actions/workflow/status/dtolnay/star-history/ci.yml?branch=master&style=for-the-badge" height="20">](https://github.com/dtolnay/star-history/actions?query=branch%3Amaster)

Command line program to generate a graph showing number of GitHub stars of a
user, org or repo over time.

```console
$ cargo install star-history
```

*Compiler support: requires rustc 1.46+*

<br>

## Screenshot

![star history of rust-lang/rust](https://user-images.githubusercontent.com/1940490/72231437-3761ff80-3570-11ea-8658-6a269feb3a21.png)

<br>

## Usage

We require a token for accessing GitHub's GraphQL API. If you have the [GitHub
CLI] (`gh`) installed, you can run `gh auth status` to find out whether a token
is already set up on your machine, and `gh auth login` if one isn't.

[GitHub CLI]: https://cli.github.com

```console
$ gh auth login

$ star-history dtolnay
$ star-history serde-rs
$ star-history rust-lang/rust
```

Simply pass multiple arguments to display multiple users or repositories on the
same graph.

The generated graphs use [D3](https://d3js.org/); the star-history command
should pop open a browser showing your graph. It uses the same mechanism that
`cargo doc --open` uses so hopefully it works well on various systems.

If you prefer not to use the `gh` CLI, you can instead provide a token to
star-history through the GITHUB\_TOKEN environment variable. Head to
<https://github.com/settings/tokens> and click "Generate new token". The default
public access permission is sufficient &mdash; you can leave all the checkboxes
empty. Save the generated token somewhere like ~/.githubtoken. Then `export
GITHUB_TOKEN=$(cat ~/.githubtoken)` prior to running star-history commands.

<br>

#### License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>
