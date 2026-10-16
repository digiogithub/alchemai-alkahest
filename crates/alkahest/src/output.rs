//! Output conventions shared by the listing commands (`ls`, `ps`, ...).
//!
//! Human output is an aligned table with upper-case headers and two spaces between columns
//! (the `ollama ls` look); `--json` switches every listing to a JSON array of objects keyed by
//! the lower-case header names. Exit codes: 0 success, 1 runtime failure, 2 usage error
//! (clap's own).

use std::io::{self, Write};

use serde_json::{Map, Value};

/// How a listing is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Aligned, human-readable table.
    Table,
    /// JSON array of objects, one per row.
    Json,
}

impl Format {
    /// Maps the global `--json` switch to a format.
    pub fn from_json_flag(json: bool) -> Self {
        if json { Self::Json } else { Self::Table }
    }
}

/// A listing: column headers plus string rows of the same width.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Listing {
    /// Creates an empty listing with the given column headers.
    pub fn new<I, S>(headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            headers: headers.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
        }
    }

    /// Appends a row. Missing cells are padded with empty strings and extra cells dropped, so a
    /// renderer never sees a ragged row.
    pub fn push_row<I, S>(&mut self, cells: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut row: Vec<String> = cells.into_iter().map(Into::into).collect();
        row.resize(self.headers.len(), String::new());
        self.rows.push(row);
    }

    /// Renders the listing in the requested format into `out`.
    pub fn render<W: Write>(&self, format: Format, out: &mut W) -> io::Result<()> {
        match format {
            Format::Table => self.render_table(out),
            Format::Json => self.render_json(out),
        }
    }

    /// Aligned table: headers upper-cased, columns padded to their widest cell, no trailing
    /// spaces on the last column.
    fn render_table<W: Write>(&self, out: &mut W) -> io::Result<()> {
        let upper: Vec<String> = self.headers.iter().map(|h| h.to_uppercase()).collect();
        let widths: Vec<usize> = (0..self.headers.len())
            .map(|i| {
                self.rows
                    .iter()
                    .map(|r| r[i].chars().count())
                    .chain(std::iter::once(upper[i].chars().count()))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        for row in std::iter::once(&upper).chain(self.rows.iter()) {
            let mut line = String::new();
            for (i, cell) in row.iter().enumerate() {
                if i > 0 {
                    line.push_str("  ");
                }
                line.push_str(cell);
                if i + 1 < row.len() {
                    let pad = widths[i] - cell.chars().count();
                    line.extend(std::iter::repeat_n(' ', pad));
                }
            }
            writeln!(out, "{line}")?;
        }
        Ok(())
    }

    /// JSON array of objects keyed by the lower-cased headers; an empty listing is `[]`.
    fn render_json<W: Write>(&self, out: &mut W) -> io::Result<()> {
        let objects: Vec<Value> = self
            .rows
            .iter()
            .map(|row| {
                let map: Map<String, Value> = self
                    .headers
                    .iter()
                    .zip(row)
                    .map(|(h, c)| (h.to_lowercase(), Value::String(c.clone())))
                    .collect();
                Value::Object(map)
            })
            .collect();
        serde_json::to_writer_pretty(&mut *out, &objects).map_err(io::Error::from)?;
        writeln!(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Listing {
        let mut l = Listing::new(["name", "size"]);
        l.push_row(["llama3:8b", "4.7 GB"]);
        l.push_row(["qwen", "1 GB"]);
        l
    }

    fn render(l: &Listing, f: Format) -> String {
        let mut buf = Vec::new();
        l.render(f, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn table_is_aligned_with_upper_case_headers() {
        assert_eq!(
            render(&sample(), Format::Table),
            "NAME       SIZE\nllama3:8b  4.7 GB\nqwen       1 GB\n"
        );
    }

    #[test]
    fn table_with_no_rows_prints_only_headers() {
        let l = Listing::new(["name", "size"]);
        assert_eq!(render(&l, Format::Table), "NAME  SIZE\n");
    }

    #[test]
    fn ragged_rows_are_padded() {
        let mut l = Listing::new(["a", "b"]);
        l.push_row(["x"]);
        assert!(render(&l, Format::Json).contains("\"b\": \"\""));
    }

    #[test]
    fn json_is_an_array_of_objects_with_lower_case_keys() {
        let v: Value = serde_json::from_str(&render(&sample(), Format::Json)).unwrap();
        assert_eq!(v[0]["name"], "llama3:8b");
        assert_eq!(v[1]["size"], "1 GB");
    }

    #[test]
    fn empty_json_listing_is_an_empty_array() {
        let l = Listing::new(["name"]);
        let v: Value = serde_json::from_str(&render(&l, Format::Json)).unwrap();
        assert_eq!(v, Value::Array(vec![]));
    }

    #[test]
    fn json_flag_selects_the_format() {
        assert_eq!(Format::from_json_flag(true), Format::Json);
        assert_eq!(Format::from_json_flag(false), Format::Table);
    }
}
