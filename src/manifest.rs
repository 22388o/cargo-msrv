use crate::errors::CargoMSRVError;
use crate::manifest::Error::UnexpectedEndOfInput;
use std::convert::TryFrom;
use std::fmt::{Display, Formatter};
use toml_edit::{Document, Item};

pub trait TomlParser {
    type Error;

    fn try_parse<T: TryFrom<Document, Error = Self::Error>>(
        &self,
        contents: &str,
    ) -> Result<T, Self::Error>;

    fn parse<T: From<Document>>(&self, contents: &str) -> Result<T, Self::Error>;
}

/// A structure for owning the values in a `Cargo.toml` manifest relevant for `cargo-msrv`.
#[derive(Debug)]
pub struct CargoManifest {
    minimum_rust_version: Option<BareVersion>,
}

impl CargoManifest {
    pub fn minimum_rust_version(&self) -> Option<&BareVersion> {
        self.minimum_rust_version.as_ref()
    }
}

/// A parser for `Cargo.toml` files. Only handles the parts necessary for `cargo-msrv`.
#[derive(Debug)]
pub struct CargoManifestParser;

impl Default for CargoManifestParser {
    fn default() -> Self {
        Self
    }
}

impl TomlParser for CargoManifestParser {
    type Error = crate::CargoMSRVError;

    fn try_parse<T: TryFrom<Document, Error = Self::Error>>(
        &self,
        contents: &str,
    ) -> Result<T, Self::Error> {
        contents
            .parse::<Document>()
            .map_err(crate::CargoMSRVError::ParseToml)
            .and_then(TryFrom::try_from)
    }

    fn parse<T: From<Document>>(&self, contents: &str) -> Result<T, Self::Error> {
        contents
            .parse()
            .map_err(crate::CargoMSRVError::ParseToml)
            .map(From::from)
    }
}

impl TryFrom<Document> for CargoManifest {
    type Error = crate::CargoMSRVError;

    fn try_from(map: Document) -> Result<Self, Self::Error> {
        let minimum_rust_version = minimum_rust_version(&map)?;

        Ok(Self {
            minimum_rust_version,
        })
    }
}

type BareVersionUsize = u64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BareVersion {
    TwoComponents(BareVersionUsize, BareVersionUsize),
    ThreeComponents(BareVersionUsize, BareVersionUsize, BareVersionUsize),
}

impl<'s> TryFrom<&'s str> for BareVersion {
    type Error = crate::CargoMSRVError;

    fn try_from(value: &'s str) -> Result<Self, Self::Error> {
        parse_bare_version(value)
    }
}

impl BareVersion {
    pub fn to_comparator(&self) -> crate::semver::Comparator {
        match self {
            Self::TwoComponents(major, minor) => crate::semver::Comparator {
                op: crate::semver::Op::Tilde,
                major: *major,
                minor: Some(*minor),
                patch: None,
                pre: crate::semver::Prerelease::EMPTY,
            },
            Self::ThreeComponents(major, minor, patch) => crate::semver::Comparator {
                op: crate::semver::Op::Tilde,
                major: *major,
                minor: Some(*minor),
                patch: Some(*patch),
                pre: crate::semver::Prerelease::EMPTY,
            },
        }
    }

    // Compared to `BareVersion::to_semver_version`, this method tries to satisfy a specified semver
    // version requirement against the given set of available version, while `BareVersion::to_semver_version`
    // simply rewrites the versions components to their semver::Version counterpart.
    pub fn try_to_semver<'s, I>(
        &self,
        iter: I,
    ) -> Result<&'s crate::semver::Version, crate::CargoMSRVError>
    where
        I: IntoIterator<Item = &'s crate::semver::Version>,
    {
        let mut iter = iter.into_iter();
        let requirements = self.to_comparator();

        iter.find(|version| requirements.matches(version))
            .ok_or_else(|| {
                let requirement = self.to_owned();
                let available = iter.map(|v| v.to_owned()).collect();
                crate::CargoMSRVError::NoVersionMatchesManifestMSRV(requirement, available)
            })
    }

    pub fn to_semver_version(&self) -> crate::semver::Version {
        match self {
            Self::TwoComponents(major, minor) => crate::semver::Version::new(*major, *minor, 0),
            Self::ThreeComponents(major, minor, patch) => {
                crate::semver::Version::new(*major, *minor, *patch)
            }
        }
    }
}

impl Display for BareVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TwoComponents(major, minor) => f.write_fmt(format_args!("{}.{}", major, minor)),
            Self::ThreeComponents(major, minor, patch) => {
                f.write_fmt(format_args!("{}.{}.{}", major, minor, patch))
            }
        }
    }
}

fn minimum_rust_version(value: &Document) -> Result<Option<BareVersion>, crate::CargoMSRVError> {
    let version = match find_minimum_rust_version(value) {
        Some(version) => version,
        None => return Ok(None),
    };

    Ok(Some(parse_bare_version(version)?))
}

#[derive(Debug, Eq, PartialEq)]
pub enum ExpectedToken {
    Number,
    Dot,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Error {
    ExpectedEndOfInput,
    Overflow,
    PreReleaseModifierNotAllowed,
    UnexpectedToken(u8, ExpectedToken),
    UnexpectedEndOfInput,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExpectedEndOfInput => write!(f, "Expected end of input"),
            Self::Overflow => write!(f, "Component would overflow"),
            Self::PreReleaseModifierNotAllowed => {
                write!(f, "Pre-release modifiers are not allowed")
            }
            Self::UnexpectedToken(c, expecteed) => write!(
                f,
                "Unexpected token '{}', expected token of kind {:?}",
                c, expecteed
            ),
            Self::UnexpectedEndOfInput => write!(f, "Unexpected end of input"),
        }
    }
}

fn parse_separator(input: &[u8]) -> Result<ParsedTokens, Error> {
    match input.iter().next() {
        Some(b'.') => Ok(1),
        Some(t) => Err(Error::UnexpectedToken(*t, ExpectedToken::Dot)),
        None => Err(UnexpectedEndOfInput),
    }
}

/// Number of tokens last parsed
type ParsedTokens = usize;

fn parse_number(input: &[u8]) -> Result<(BareVersionUsize, ParsedTokens), Error> {
    let mut out: BareVersionUsize = 0;
    let mut len = 0;

    const ZERO_MIN: u8 = b'0' - 1;
    const NINE_PLUS: u8 = b'9' + 1;

    while let Some(token) = input.get(len) {
        match token {
            b'0'..=b'9' => {
                out = out.checked_mul(10).ok_or(Error::Overflow)?;
                out = out
                    .checked_add((*token - b'0') as BareVersionUsize)
                    .ok_or(Error::Overflow)?;

                len += 1;
            }
            0u8..=ZERO_MIN | NINE_PLUS..=u8::MAX => {
                break;
            }
        }
    }

    match len {
        0 => Err(Error::UnexpectedEndOfInput),
        _ => Ok((out, len as usize)),
    }
}

fn expect_end_of_input(input: &[u8]) -> Result<(), Error> {
    if input.is_empty() {
        Ok(())
    } else {
        Err(Error::ExpectedEndOfInput)
    }
}

/// Parse the [`bare version`] which defines a minimal supported Rust version (MSRV or rust-version
/// in `Cargo.toml`).
///
/// See also the [`semver 2.0 spec`], which the parser is loosely based on. NB: a `bare version` is
/// not `semver` compatible.
///
/// [`bare version`]: https://doc.rust-lang.org/nightly/cargo/reference/manifest.html#the-rust-version-field
/// [`semver 2.0 spec`]: https://semver.org/spec/v2.0.0.html#backusnaur-form-grammar-for-valid-semver-versions
fn parse_bare_version(input: &str) -> Result<BareVersion, crate::CargoMSRVError> {
    let input = input.as_bytes();
    let mut parsed_tokens = 0;

    let (major, tokens) = parse_number(input)?;
    parsed_tokens += tokens;

    let tokens = parse_separator(&input[parsed_tokens..])?;
    parsed_tokens += tokens;

    let (minor, tokens) = parse_number(&input[parsed_tokens..])?;
    parsed_tokens += tokens;

    if expect_end_of_input(&input[parsed_tokens..]).is_ok() {
        return Ok(BareVersion::TwoComponents(major, minor));
    }

    let tokens = parse_separator(&input[parsed_tokens..])?;
    parsed_tokens += tokens;

    let (patch, tokens) = parse_number(&input[parsed_tokens..])?;
    parsed_tokens += tokens;

    if expect_end_of_input(&input[parsed_tokens..]).is_ok() {
        return Ok(BareVersion::ThreeComponents(major, minor, patch));
    }

    // Like Cargo, we disallow pre-release modifiers.
    // https://github.com/rust-lang/cargo/blob/ec38c84ab1d257c9d0129bd9cf7eade1d511a8d2/src/cargo/util/toml/mod.rs#L1117-L1132
    if input[parsed_tokens..].starts_with(&[b'-']) {
        return Err(CargoMSRVError::BareVersionParse(
            Error::PreReleaseModifierNotAllowed,
        ));
    }

    Err(CargoMSRVError::BareVersionParse(Error::ExpectedEndOfInput))
}

/// Parse the minimum supported Rust version (MSRV) from `Cargo.toml` manifest data.
fn find_minimum_rust_version(document: &Document) -> Option<&str> {
    /// Parses the `MSRV` as supported by Cargo since Rust 1.56.0
    ///
    /// [`Cargo`]: https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field
    fn find_rust_version(document: &Document) -> Option<&str> {
        document
            .as_table()
            .get("package")
            .and_then(Item::as_table)
            .and_then(|package| package.get("rust-version"))
            .and_then(|rust_version| rust_version.as_str())
    }

    /// Parses the MSRV as supported by `cargo-msrv`, since prior to the release of Rust
    /// 1.56.0
    fn find_metadata_msrv(document: &Document) -> Option<&str> {
        document
            .as_table()
            .get("package")
            .and_then(Item::as_table)
            .and_then(|package| package.get("metadata"))
            .and_then(Item::as_table)
            .and_then(|metadata| metadata.get("msrv"))
            .and_then(|msrv| msrv.as_str())
    }

    // Parse the MSRV from the `package.rust-version` key if it exists,
    // and try to fallback to our own `package.metadata.msrv` if it doesn't
    find_rust_version(document).or_else(|| find_metadata_msrv(document))
}

#[cfg(test)]
mod minimal_version_tests {
    use crate::errors::CargoMSRVError;
    use crate::manifest::{BareVersion, CargoManifest, CargoManifestParser, Error, TomlParser};
    use std::convert::TryFrom;
    use toml_edit::Document;

    #[test]
    fn parse_toml() {
        let contents = r#"[package]
name = "some"
version = "0.1.0"
edition = "2018"

[dependencies]
"#;

        assert!(CargoManifestParser::default()
            .parse::<Document>(contents)
            .is_ok());
    }

    #[test]
    fn parse_invalid_toml() {
        let contents = r#"-[package]
name = "some"
version = "0.1.0"
edition = "2018"

[dependencies]
"#;

        assert!(CargoManifestParser::default()
            .parse::<Document>(contents)
            .is_err());
    }

    #[test]
    fn parse_no_minimum_rust_version() {
        let contents = r#"[package]
name = "some"
version = "0.1.0"
edition = "2018"

[dependencies]
"#;

        let manifest = CargoManifestParser::default()
            .parse::<Document>(contents)
            .unwrap();

        let manifest = CargoManifest::try_from(manifest).unwrap();

        assert!(manifest.minimum_rust_version.is_none());
    }

    #[test]
    fn parse_rust_version_three_components() {
        let contents = r#"[package]
name = "some"
version = "0.1.0"
edition = "2018"
rust-version = "1.56.0"

[dependencies]
"#;

        let manifest = CargoManifestParser::default()
            .parse::<Document>(contents)
            .unwrap();

        let manifest = CargoManifest::try_from(manifest).unwrap();
        let version = manifest.minimum_rust_version.unwrap();

        assert_eq!(version, BareVersion::ThreeComponents(1, 56, 0));
    }

    #[test]
    fn parse_rust_version_three_components_with_pre_release() {
        let contents = r#"[package]
name = "some"
version = "0.1.0"
edition = "2018"
rust-version = "1.56.0-nightly"

[dependencies]
"#;

        let manifest = CargoManifestParser::default()
            .parse::<Document>(contents)
            .unwrap();

        let parse_err = CargoManifest::try_from(manifest).unwrap_err();

        if let CargoMSRVError::BareVersionParse(err) = parse_err {
            assert_eq!(err, Error::PreReleaseModifierNotAllowed);
        } else {
            panic!("Incorrect cargo-msrv error type")
        }
    }

    #[test]
    fn parse_rust_version_two_components() {
        let contents = r#"[package]
name = "some"
version = "0.1.0"
edition = "2018"
rust-version = "1.56"

[dependencies]
"#;

        let manifest = CargoManifestParser::default()
            .parse::<Document>(contents)
            .unwrap();

        let manifest = CargoManifest::try_from(manifest).unwrap();
        let version = manifest.minimum_rust_version.unwrap();

        assert_eq!(version, BareVersion::TwoComponents(1, 56));
    }

    #[yare::parameterized(
        empty = {""},
        one_component = {"1"},
        one_component_dot = {"1."},
        two_components_dot = {"1.1."},
        three_components_dot = {"1.1.1."},
        two_components_with_pre_release = {"1.1-nightly"},
        two_components_not_a_number = {"1.x"},
        three_components_not_a_number = {"1.1.x"},
        too_many_components = {"1.1.0.0"},
    )]
    fn parse_rust_version_faulty_versions(version: &str) {
        let contents = format!(
            r#"[package]
name = "some"
version = "0.1.0"
edition = "2018"
rust-version = "{}"

[dependencies]
"#,
            version
        );

        let manifest = CargoManifestParser::default()
            .parse::<Document>(&contents)
            .unwrap();

        let manifest = CargoManifest::try_from(manifest);

        assert!(manifest.is_err())
    }

    #[test]
    fn parse_metadata_msrv_three_components() {
        let contents = r#"[package]
name = "some"
version = "0.1.0"
edition = "2018"

[package.metadata]
msrv = "1.51.0"

[dependencies]
"#;

        let manifest = CargoManifestParser::default()
            .parse::<Document>(contents)
            .unwrap();

        let manifest = CargoManifest::try_from(manifest).unwrap();
        let version = manifest.minimum_rust_version.unwrap();

        assert_eq!(version, BareVersion::ThreeComponents(1, 51, 0));
    }

    #[test]
    fn parse_metadata_msrv_two_components() {
        let contents = r#"[package]
name = "some"
version = "0.1.0"
edition = "2018"

[package.metadata]
msrv = "1.51"

[dependencies]
"#;

        let manifest = CargoManifestParser::default()
            .parse::<Document>(contents)
            .unwrap();

        let manifest = CargoManifest::try_from(manifest).unwrap();
        let version = manifest.minimum_rust_version.unwrap();

        assert_eq!(version, BareVersion::TwoComponents(1, 51));
    }

    #[yare::parameterized(
        empty = {""},
        one_component = {"1"},
        one_component_dot = {"1."},
        two_components_dot = {"1.1."},
        three_components_dot = {"1.1.1."},
        two_components_with_pre_release = {"1.1-nightly"},
        two_components_not_a_number = {"1.x"},
        three_components_not_a_number = {"1.1.x"},
        too_many_components = {"1.1.0.0"},
    )]
    fn parse_metadata_msrv_faulty_versions(version: &str) {
        let contents = format!(
            r#"[package]
name = "some"
version = "0.1.0"
edition = "2018"

[package.metadata]
msrv = "{}"

[dependencies]
"#,
            version
        );

        let manifest = CargoManifestParser::default()
            .parse::<Document>(&contents)
            .unwrap();

        let manifest = CargoManifest::try_from(manifest);

        assert!(manifest.is_err())
    }
}

#[cfg(test)]
mod bare_version_tests {
    use crate::manifest::BareVersion;
    use rust_releases::{semver, Release, ReleaseIndex};
    use std::iter::FromIterator;
    use yare::parameterized;

    fn release_indices() -> ReleaseIndex {
        FromIterator::from_iter(vec![
            Release::new_stable(semver::Version::new(2, 56, 0)),
            Release::new_stable(semver::Version::new(1, 56, 0)),
            Release::new_stable(semver::Version::new(1, 55, 0)),
            Release::new_stable(semver::Version::new(1, 54, 2)),
            Release::new_stable(semver::Version::new(1, 54, 1)),
            Release::new_stable(semver::Version::new(1, 0, 0)),
        ])
    }

    #[parameterized(
        two_component_two_fifty_six = { "2.56", BareVersion::TwoComponents(2, 56) },
        three_component_two_fifty_six = { "2.56.0", BareVersion::ThreeComponents(2, 56, 0) },
        two_component_one_fifty_five = { "1.55", BareVersion::TwoComponents(1, 55) },
        three_component_one_fifty_five = { "1.55.0", BareVersion::ThreeComponents(1, 55, 0) },
        three_component_one_fifty_four = { "1.54.0", BareVersion::ThreeComponents(1, 54, 0) },
        three_component_one_fifty_four_p1 = { "1.54.1", BareVersion::ThreeComponents(1, 54, 1) },
        three_component_one_fifty_four_p10 = { "1.54.10", BareVersion::ThreeComponents(1, 54, 10) },
        two_component_zeros = { "0.0", BareVersion::TwoComponents(0, 0) },
        three_component_zeros = { "0.0.0", BareVersion::ThreeComponents(0, 0, 0) },
        two_component_large_major = { "18446744073709551615.0", BareVersion::TwoComponents(18446744073709551615, 0) },
        two_component_large_minor = { "0.18446744073709551615", BareVersion::TwoComponents(0, 18446744073709551615) },
        three_component_large_major = { "18446744073709551615.0.0", BareVersion::ThreeComponents(18446744073709551615, 0, 0) },
        three_component_large_minor = { "0.18446744073709551615.0", BareVersion::ThreeComponents(0, 18446744073709551615, 0) },
        three_component_large_patch = { "0.0.18446744073709551615", BareVersion::ThreeComponents(0, 0, 18446744073709551615) },

    )]
    fn try_from_ok(version: &str, expected: BareVersion) {
        use std::convert::TryFrom;

        let version = BareVersion::try_from(version).unwrap();

        assert_eq!(version, expected);
    }

    #[parameterized(
        empty = { "" }, // no first component
        no_components_space = { "1 36 0" },
        no_components_comma = { "1,36,0" },
        first_component_nan = { "x.0.0" },
        no_second_component = { "1." },
        second_component_nan = { "1.x" },
        no_third_component = { "1.0." },
        third_component_nan = { "1.36.x" },
        too_large_int_major_2c = { "18446744073709551616.0" },
        too_large_int_minor_2c = { "0.18446744073709551616" },
        too_large_int_major_3c = { "18446744073709551616.0.0" },
        too_large_int_minor_3c = { "0.18446744073709551616.0" },
        too_large_int_patch_3c = { "0.0.18446744073709551616" },        
        neg_int_major = { "-1.0.0" },
        neg_int_minor = { "0.-1.0" },
        neg_int_patch = { "0.0.-1" },
        build_postfix_without_pre_release_id = { "0.0.0+some" },
        two_component_pre_release_id_variant_1 = { "0.0-nightly" },
        two_component_pre_release_id_variant_2 = { "0.0-beta.0" },
        two_component_pre_release_id_variant_3 = { "0.0-beta.1" },
        two_component_pre_release_id_variant_4 = { "0.0-anything", },
        two_component_pre_release_id_variant_5 = { "0.0-anything+build" },
        three_component_pre_release_id_variant_2 = { "0.0.0-beta.0" },
        three_component_pre_release_id_variant_3 = { "0.0.0-beta.1" },
        three_component_pre_release_id_variant_1 = { "0.0.0-nightly" },
        three_component_pre_release_id_variant_4 = { "0.0.0-anything" },
        three_component_pre_release_id_variant_5 = { "0.0.0-anything+build" },
    )]
    fn try_from_err(version: &str) {
        use std::convert::TryFrom;

        let res = BareVersion::try_from(version);

        assert!(res.is_err());
    }

    #[parameterized(
        two_fifty_six = {  BareVersion::TwoComponents(2, 56), semver::Version::new(2, 56, 0) },
        one_fifty_six = {  BareVersion::TwoComponents(1, 56), semver::Version::new(1, 56, 0) },
        one_fifty_five = {  BareVersion::TwoComponents(1, 55), semver::Version::new(1, 55, 0) },
        one_fifty_four_p2 = {  BareVersion::TwoComponents(1, 54), semver::Version::new(1, 54, 2) },
        one_fifty_four_p1 = {  BareVersion::TwoComponents(1, 54), semver::Version::new(1, 54, 2) },
        one_fifty_four_p0 = {  BareVersion::TwoComponents(1, 54), semver::Version::new(1, 54, 2) },
        one = {  BareVersion::TwoComponents(1, 0), semver::Version::new(1, 0, 0) },
    )]
    fn two_components_to_semver(version: BareVersion, expected: semver::Version) {
        let index = release_indices();
        let available = index.releases().iter().map(|release| release.version());

        let v = version.try_to_semver(available).unwrap();

        assert_eq!(v, &expected);
    }

    #[parameterized(
        two_fifty_six = {  BareVersion::ThreeComponents(2, 56, 0), semver::Version::new(2, 56, 0) },
        one_fifty_six = {  BareVersion::ThreeComponents(1, 56, 0), semver::Version::new(1, 56, 0) },
        one_fifty_five = {  BareVersion::ThreeComponents(1, 55, 0), semver::Version::new(1, 55, 0) },
        one_fifty_four_p2 = {  BareVersion::ThreeComponents(1, 54, 2), semver::Version::new(1, 54, 2) },
        one_fifty_four_p1 = {  BareVersion::ThreeComponents(1, 54, 1), semver::Version::new(1, 54, 2) },
        one_fifty_four_p0 = {  BareVersion::ThreeComponents(1, 54, 0), semver::Version::new(1, 54, 2) },
        one = {  BareVersion::ThreeComponents(1, 0, 0), semver::Version::new(1, 0, 0) },
    )]
    fn three_components_to_semver(version: BareVersion, expected: semver::Version) {
        let index = release_indices();
        let available = index.releases().iter().map(|release| release.version());

        let v = version.try_to_semver(available).unwrap();

        assert_eq!(v, &expected);
    }
}
