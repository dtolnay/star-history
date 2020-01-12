GitHub star history
===================

[![Build Status](https://api.travis-ci.com/dtolnay/star-history.svg?branch=master)](https://travis-ci.com/dtolnay/star-history)
[![Latest Version](https://img.shields.io/crates/v/star-history.svg)](https://crates.io/crates/star-history)

Command line program to generate a graph showing number of GitHub stars of a
user or repo over time.

```console
$ cargo install star-history
```

*Compiler support: requires rustc 1.40+*

<br>

## Screenshot

![star history of rust-lang/rust](https://user-images.githubusercontent.com/1940490/72231437-3761ff80-3570-11ea-8658-6a269feb3a21.png)

<br>

## Usage

We require a token for accessing GitHub's GraphQL API. Head to
https://github.com/settings/tokens and click "Generate new token". The default
public access permission is sufficient &mdash; you can leave all the checkboxes
empty. Save the generated token somewhere like ~/.githubtoken.

Then:

```console
$ export GITHUB_TOKEN=$(cat ~/.githubtoken)

$ star-history dtolnay
$ star-history rust-lang/rust
```

Simply pass multiple arguments to display multiple users or repositories on the
same graph.

The generated graphs use [D3](https://d3js.org/); the star-history command
should pop open a browser showing your graph. It uses the same mechanism that
`cargo doc --open` uses so hopefully it works well on various systems.

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
