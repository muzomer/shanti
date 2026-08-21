//! Machine-readable output from jj, via its template language.
//!
//! jj's human-facing output is formatted for people: aligned, abbreviated and
//! free to change between releases. Its template language is the documented,
//! stable contract, so every read shanti performs names the exact fields it
//! wants and gets them back one record per line.

use color_eyre::eyre::{self, eyre};

/// Field delimiter inside a record: ASCII unit separator (U+001F).
///
/// Chosen because it cannot occur in the values jj emits (bookmark names,
/// change ids, paths, timestamps) and needs no quoting or escaping, so parsing
/// stays a plain `split`. Tabs and commas do not have that property — a commit
/// description or a path may legitimately contain either.
pub const FIELD_SEPARATOR: char = '\u{1f}';

/// A record separator, kept explicit for the same reason as the field one.
pub const RECORD_SEPARATOR: char = '\n';

/// A jj template that yields one line per record, with named fields.
///
/// The names are shanti's own labels for the columns; the expressions are jj
/// template syntax. Keeping them paired means a record can be read by name and
/// that the arity check below actually knows what is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    fields: &'static [(&'static str, &'static str)],
}

impl Template {
    /// Build a template from `(name, jj expression)` pairs, in output order.
    pub const fn new(fields: &'static [(&'static str, &'static str)]) -> Self {
        Self { fields }
    }

    /// The template expression to hand to `jj -T`.
    ///
    /// The separators are embedded as literal characters inside the template's
    /// string literals rather than as escape sequences: jj's set of recognised
    /// escapes has changed over time, whereas a raw byte in a quoted string has
    /// always meant itself.
    pub fn expression(&self) -> String {
        let mut expression = String::new();
        for (_, value) in self.fields {
            if !expression.is_empty() {
                expression.push_str(&format!(" ++ \"{FIELD_SEPARATOR}\" ++ "));
            }
            expression.push_str(value);
        }
        // Without a trailing newline `--no-graph` runs every record together.
        expression.push_str(&format!(" ++ \"{RECORD_SEPARATOR}\""));
        expression
    }

    /// Field names, in output order.
    pub fn field_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.fields.iter().map(|(name, _)| *name)
    }

    /// Split raw jj output into records.
    ///
    /// A wrong field count is treated as an error rather than as missing data:
    /// it means jj rendered something other than what this template asked for —
    /// almost always a version skew — and guessing would push the confusion
    /// downstream into whichever field happened to shift.
    pub fn parse(&self, output: &str) -> eyre::Result<Vec<Record>> {
        output
            .split(RECORD_SEPARATOR)
            // jj terminates the last record too, so the tail is empty.
            .filter(|line| !line.is_empty())
            .map(|line| self.parse_record(line))
            .collect()
    }

    fn parse_record(&self, line: &str) -> eyre::Result<Record> {
        let values: Vec<String> = line.split(FIELD_SEPARATOR).map(str::to_owned).collect();
        if values.len() != self.fields.len() {
            return Err(eyre!(
                "jj returned {} field(s) where the template asked for {} ({}); \
                 this usually means the installed jj renders this template differently",
                values.len(),
                self.fields.len(),
                self.field_names().collect::<Vec<_>>().join(", ")
            ));
        }
        Ok(Record {
            fields: self.fields,
            values,
        })
    }
}

/// One line of template output, addressable by field name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    fields: &'static [(&'static str, &'static str)],
    values: Vec<String>,
}

impl Record {
    /// The value of `name`, or an error naming the template's own fields —
    /// a caller typo and a template drift look the same from here, and both
    /// deserve to say which names do exist.
    pub fn get(&self, name: &str) -> eyre::Result<&str> {
        self.fields
            .iter()
            .position(|(field, _)| *field == name)
            .map(|index| self.values[index].as_str())
            .ok_or_else(|| eyre!("no field {name:?} in this jj record"))
    }

    /// Values in output order, for callers that would rather destructure.
    pub fn values(&self) -> &[String] {
        &self.values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACES: Template = Template::new(&[
        ("name", "name"),
        ("change_id", "target.change_id().short()"),
        ("empty", "target.empty()"),
    ]);

    #[test]
    fn joins_fields_with_the_unit_separator_and_ends_the_record() {
        assert_eq!(
            WORKSPACES.expression(),
            format!(
                "name ++ \"{sep}\" ++ target.change_id().short() ++ \"{sep}\" ++ target.empty() ++ \"\n\"",
                sep = FIELD_SEPARATOR
            )
        );
    }

    #[test]
    fn parses_one_record_per_line() {
        let output = format!(
            "default{s}qpvunt{s}false\nfeature{s}zzzzzz{s}true\n",
            s = FIELD_SEPARATOR
        );
        let records = WORKSPACES.parse(&output).unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].get("name").unwrap(), "default");
        assert_eq!(records[1].get("change_id").unwrap(), "zzzzzz");
        assert_eq!(records[1].get("empty").unwrap(), "true");
    }

    #[test]
    fn keeps_empty_fields_rather_than_dropping_them() {
        // An absent bookmark renders as the empty string; it is data, not noise.
        let output = format!("default{s}{s}false\n", s = FIELD_SEPARATOR);
        let records = WORKSPACES.parse(&output).unwrap();
        assert_eq!(records[0].get("change_id").unwrap(), "");
    }

    #[test]
    fn empty_output_is_no_records_not_an_error() {
        assert!(WORKSPACES.parse("").unwrap().is_empty());
    }

    #[test]
    fn a_field_count_mismatch_names_the_expected_fields() {
        let output = format!("default{s}qpvunt\n", s = FIELD_SEPARATOR);
        let error = WORKSPACES.parse(&output).unwrap_err().to_string();

        assert!(error.contains("2 field(s)"), "{error}");
        assert!(error.contains("name, change_id, empty"), "{error}");
    }

    #[test]
    fn unknown_field_names_are_rejected() {
        let output = format!("default{s}qpvunt{s}false\n", s = FIELD_SEPARATOR);
        let records = WORKSPACES.parse(&output).unwrap();
        assert!(records[0].get("nonexistent").is_err());
    }
}
