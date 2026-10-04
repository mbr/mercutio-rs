//! Reconstructs protocol inputs from parsed command options and whole-input JSON.

use std::{io::Read, path::PathBuf};

use clap::ArgMatches;
use serde_json::{Map, Value};

use super::{
    CliError, CliErrorKind, ImageMode, OutputMode, OutputOptions,
    schema::{NodeSpec, ObjectSpec, ScalarKind, UnionSpec, ValueKind, ValueSpec},
};

impl ObjectSpec {
    /// Returns whether any descendant option was supplied.
    fn has_input(&self, matches: &ArgMatches) -> bool {
        self.children.iter().any(|child| child.has_input(matches))
    }

    /// Reconstructs this object when selected or required.
    pub(super) fn reconstruct(
        &self,
        matches: &ArgMatches,
        parent_active: bool,
    ) -> Result<Option<Value>, String> {
        let active =
            self.name.is_none() || (self.required && parent_active) || self.has_input(matches);
        if !active {
            return Ok(None);
        }

        let mut object = Map::new();
        for child in &self.children {
            if let Some((name, value)) = child.reconstruct(matches, true)? {
                object.insert(name, value);
            }
        }
        Ok(Some(Value::Object(object)))
    }
}

impl NodeSpec {
    /// Returns whether this node has supplied input.
    fn has_input(&self, matches: &ArgMatches) -> bool {
        match self {
            Self::Object(object) => object.has_input(matches),
            Self::Value(value) => value.is_present(matches),
            Self::Union(union) => {
                union.scalar.is_present(matches) || union.object.has_input(matches)
            }
        }
    }

    /// Reconstructs this node and its original property name.
    fn reconstruct(
        &self,
        matches: &ArgMatches,
        parent_active: bool,
    ) -> Result<Option<(String, Value)>, String> {
        match self {
            Self::Object(object) => Ok(object
                .reconstruct(matches, parent_active)?
                .map(|value| (object.name.clone().expect("child object has a name"), value))),
            Self::Value(spec) => spec
                .reconstruct(matches, parent_active)
                .map(|result| result.map(|value| (spec.name.clone(), value))),
            Self::Union(union) => union.reconstruct(matches, parent_active),
        }
    }
}

impl ValueSpec {
    /// Returns whether this option occurred.
    fn is_present(&self, matches: &ArgMatches) -> bool {
        matches.value_source(&self.cli_name).is_some()
    }

    /// Reconstructs this option's JSON value.
    fn reconstruct(
        &self,
        matches: &ArgMatches,
        parent_active: bool,
    ) -> Result<Option<Value>, String> {
        let Some(values) = matches.get_many::<String>(&self.cli_name) else {
            if self.required && parent_active {
                return Err(format!("missing required option `--{}`", self.cli_name));
            }
            return Ok(None);
        };

        let first = values.clone().next().expect("supplied option has a value");
        match &self.kind {
            ValueKind::Scalar(kind) => parse_scalar(kind, first, &self.cli_name).map(Some),
            ValueKind::Array(kind) => values
                .map(|value| parse_scalar(kind, value, &self.cli_name))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
                .map(Some),
            ValueKind::Json(expected) => {
                let value: Value = serde_json::from_str(first)
                    .map_err(|error| format!("invalid JSON for `--{}`: {error}", self.cli_name))?;
                if let Some(expected) = expected
                    && !json_has_type(&value, expected)
                {
                    return Err(format!(
                        "invalid JSON for `--{}`: expected {expected}",
                        self.cli_name
                    ));
                }
                Ok(Some(value))
            }
        }
    }
}

impl UnionSpec {
    /// Reconstructs exactly one selected union branch.
    fn reconstruct(
        &self,
        matches: &ArgMatches,
        parent_active: bool,
    ) -> Result<Option<(String, Value)>, String> {
        let scalar_selected = self.scalar.is_present(matches);
        let object_selected = self.object.has_input(matches);
        if scalar_selected && object_selected {
            return Err(format!(
                "`--{}` cannot be combined with its object branch options",
                self.scalar.cli_name
            ));
        }
        if scalar_selected {
            let value = self
                .scalar
                .reconstruct(matches, true)?
                .expect("selected scalar union branch has a value");
            return Ok(Some((self.name.clone(), value)));
        }
        if object_selected {
            let value = self
                .object
                .reconstruct(matches, true)?
                .expect("selected object union branch is active");
            return Ok(Some((self.name.clone(), value)));
        }
        if self.required && parent_active {
            return Err(format!(
                "one of `--{}` or its object branch options is required",
                self.scalar.cli_name
            ));
        }
        Ok(None)
    }
}

/// Parses and validates root output options.
pub(super) fn parse_output_options(matches: &ArgMatches) -> Result<OutputOptions, String> {
    let mode = match matches
        .get_one::<String>("output-mode")
        .map(String::as_str)
        .expect("output mode has a default")
    {
        "artifacts" => OutputMode::Artifacts,
        "structured" => OutputMode::Structured,
        "raw" => OutputMode::Raw,
        "binary" => OutputMode::Binary,
        _ => unreachable!("Clap validates output modes"),
    };
    let images = match matches
        .get_one::<String>("image-mode")
        .map(String::as_str)
        .expect("image mode has a default")
    {
        "auto" => ImageMode::Auto,
        "kitty" => ImageMode::Kitty,
        "off" => ImageMode::Off,
        _ => unreachable!("Clap validates image modes"),
    };
    let artifact_dir = matches.get_one::<String>("artifact-dir").map(PathBuf::from);
    if mode != OutputMode::Artifacts
        && (artifact_dir.is_some()
            || matches.value_source("image-mode") == Some(clap::parser::ValueSource::CommandLine))
    {
        return Err("--artifact-dir and --images require --output artifacts".into());
    }
    Ok(OutputOptions {
        mode,
        images,
        artifact_dir,
    })
}

/// Reads exactly one whole-input JSON object.
pub(super) fn read_json_input(input: &mut impl Read) -> Result<Value, CliError> {
    let value: Value = serde_json::from_reader(input).map_err(|error| {
        if error.is_io() {
            CliError::runtime(format!("failed to read JSON input: {error}"))
        } else {
            CliError {
                kind: CliErrorKind::Usage,
                message: format!("error: invalid JSON input: {error}\n"),
            }
        }
    })?;
    if !value.is_object() {
        return Err(CliError {
            kind: CliErrorKind::Usage,
            message: "error: whole-input JSON must be an object\n".into(),
        });
    }
    Ok(value)
}

/// Parses one strict scalar value.
fn parse_scalar(kind: &ScalarKind, input: &str, cli_name: &str) -> Result<Value, String> {
    match kind {
        ScalarKind::String(_) => Ok(Value::String(input.to_string())),
        ScalarKind::Boolean => match input {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(format!("invalid boolean for `--{cli_name}`")),
        },
        ScalarKind::Integer | ScalarKind::Number => {
            let expected = if matches!(kind, ScalarKind::Integer) {
                "integer"
            } else {
                "number"
            };
            let value: Value = serde_json::from_str(input)
                .map_err(|error| format!("invalid {expected} for `--{cli_name}`: {error}"))?;
            if !json_has_type(&value, expected) {
                return Err(format!("invalid {expected} for `--{cli_name}`"));
            }
            Ok(value)
        }
    }
}

/// Returns whether a JSON value has the expected basic type.
fn json_has_type(value: &Value, expected: &str) -> bool {
    match expected {
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "object" => value.is_object(),
        "string" => value.is_string(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    //! Exercises reader failures at JSON document boundaries.

    use std::io::{self, Read};

    use super::read_json_input;

    /// Reports an I/O failure instead of EOF.
    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected read failure"))
        }
    }

    /// Keeps stream failures distinct from usage errors, including after a complete value.
    #[test]
    fn read_failures_are_runtime_errors() {
        for prefix in [b"".as_slice(), b"{", b"{}"] {
            let error = read_json_input(&mut prefix.chain(FailingReader))
                .expect_err("reader fails before EOF");
            assert_eq!(error.exit_code(), 1);
        }
    }
}
