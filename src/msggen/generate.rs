use std::io;

use super::parser::{ArraySpecifier, BaseTypeName, Comment, Item, TypeName, Value};

/// Where a generated line goes
#[derive(Clone, Copy, PartialEq)]
enum Target {
  /// Above everything
  Header,
  /// Inside the `impl` block for constants
  Constants,
  /// Inside the struct body
  Fields,
}

pub fn print_struct_definition<W: io::Write>(
  w: &mut W,
  name: &str,
  lines: &[(Option<Item>, Option<Comment>)],
) -> io::Result<()> {
  // Collect constants and fields into separate
  // buffers, so that all constants end up in an `impl` block and all fields in
  // the struct, regardless of their order in the input.
  //
  // Comments at the end of an item or preceeding comment-only lines become
  // doc comments for that item.
  // An empty line ends a comment block, so it will not be attached as doc comment to the next
  // item. Such a block is emitted as a plain comment after the previous item.
  // A comment block followed by an empty line at the very top of the message file is attached to
  // the generated struct.
  let mut header: Vec<String> = Vec::new();
  let mut constants: Vec<String> = Vec::new();
  let mut fields: Vec<String> = Vec::new();

  // Comment lines since the last item or empty line, held back until we know
  // which item, and therefore which buffer, they belong to.
  let mut pending: Vec<&str> = Vec::new();
  // Where the most recent item went, i.e. where anything that documents no
  // following item belongs to.
  let mut previous_target = Target::Header;

  // We only produce defaults for messages where each field has a default value,
  // otherwise we would need to define constructors instead of just implementing
  // Default. If we encounter any field without a default value, we set
  // `defaults` to None, and skip all default values.
  let mut defaults = Some(Vec::new());

  // Pretend the input ends with an empty line, so that comments trailing the last
  // item are flushed like any other comment block. The extra empty line is
  // trimmed away below.
  let end_of_input = (None, None);
  for (item, comment) in lines.iter().chain(std::iter::once(&end_of_input)) {
    let (target, line) = match (item, comment) {
      (None, Some(Comment(c))) => {
        pending.push(c);
        continue;
      }
      (None, None) => {
        // An empty line ends a comment block.
        // It becomes a plain comment next to the previous item.
        // (Or a doc comment for the struct if at the beginning of the file.)
        if !pending.is_empty() {
          let (buffer, prefix) = match previous_target {
            Target::Header => (&mut header, "///"),
            Target::Constants => (&mut constants, "//"),
            Target::Fields => (&mut fields, "//"),
          };
          buffer.extend(pending.drain(..).map(|c| format!("{prefix}{c}")));
          continue;
        }
        (previous_target, String::new())
      }
      (
        Some(Item::Constant {
          type_name,
          const_name,
          value,
        }),
        comment,
      ) => {
        if let Some(Comment(c)) = comment {
          pending.push(c);
        }
        let rust_type = translate_type(type_name)?;
        let rust_value = translate_value(value, &rust_type);
        let line = format!("pub const {const_name}: {rust_type} = {rust_value};");
        (Target::Constants, line)
      }
      (
        Some(Item::Field {
          type_name,
          field_name,
          default_value,
        }),
        comment,
      ) => {
        if let Some(Comment(c)) = comment {
          pending.push(c);
        }
        let rust_type = translate_type(type_name)?;
        let mut line = format!("pub {} : {},", escape_keywords(field_name), rust_type);
        if let Some(defaults_vec) = defaults.as_mut() {
          if let Some(default_value) = default_value {
            let rust_value = translate_value(default_value, &rust_type);
            defaults_vec.push(format!("{}: {rust_value}", escape_keywords(field_name)));
          } else {
            if !defaults_vec.is_empty() {
              line.push_str(&format!(
                "// no default value for field {field_name}, skipping previous defaults"
              ));
            }
            defaults = None;
          }
        } else if default_value.is_some() {
          line.push_str(&format!(
            "// no default value for a previous field, skipping default value for field \
             {field_name}"
          ));
        }
        (Target::Fields, line)
      }
    };

    let buffer = match target {
      Target::Header => &mut header,
      Target::Constants => &mut constants,
      Target::Fields => &mut fields,
    };
    buffer.extend(pending.drain(..).map(|c| format!("///{c}")));
    buffer.push(line);
    previous_target = target;
  }

  // Trailing empty lines are of no use in the generated code.
  for buffer in [&mut header, &mut constants, &mut fields] {
    while buffer.last().is_some_and(String::is_empty) {
      buffer.pop();
    }
  }

  for line in header {
    writeln!(w, "{line}")?;
  }

  writeln!(w, "#[derive(Debug, Serialize, Deserialize, Clone)]")?;
  writeln!(w, "pub struct {name} {{")?;
  for line in fields {
    if line.is_empty() {
      writeln!(w)?;
    } else {
      writeln!(w, "  {line}")?;
    }
  }
  writeln!(w, "}}")?;

  if !constants.is_empty() {
    writeln!(w, "impl {name} {{")?;
    for line in constants {
      if line.is_empty() {
        writeln!(w)?;
      } else {
        writeln!(w, "  {line}")?;
      }
    }
    writeln!(w, "}}")?;
  }

  if let Some(defaults) = defaults {
    if !defaults.is_empty() {
      writeln!(w, "impl Default for {name} {{")?;
      writeln!(w, "  fn default() -> Self {{")?;
      writeln!(w, "    Self {{")?;
      for field in defaults {
        writeln!(w, "      {field},")?;
      }
      writeln!(w, "    }}")?;
      writeln!(w, "  }}")?;
      writeln!(w, "}}")?;
    }
  }
  Ok(())
}

fn escape_keywords(id: &str) -> String {
  match id {
    "type" => {
      let mut s = "r#".to_string();
      s.push_str(id);
      s
    }
    _ => id.to_string(),
  }
}

const RUST_BYTESTRING: &str = "std::string::String";
const RUST_WIDE_STRING: &str = "WString";

fn translate_type(t: &TypeName) -> io::Result<String> {
  let mut base = String::new();
  match t.base {
    BaseTypeName::Primitive { ref name } => base.push_str(match name.as_str() {
      "bool" => "bool",
      "byte" => "u8",
      "char" => "u8",
      "float32" => "f32",
      "float64" => "f64",
      "int8" => "i8",
      "int16" => "i16",
      "int32" => "i32",
      "int64" => "i64",
      "uint8" => "u8",
      "uint16" => "u16",
      "uint32" => "u32",
      "uint64" => "u64",
      "string" => RUST_BYTESTRING,
      "wstring" => RUST_WIDE_STRING,
      other => panic!("Unexpected primitive type {}", other),
    }),
    BaseTypeName::BoundedString { .. } => base.push_str(RUST_BYTESTRING), /* We do not have type */
    // to represent
    // boundedness
    BaseTypeName::ComplexType {
      ref package_name,
      ref type_name,
    } => {
      if let Some(pkg) = package_name {
        base.push_str("super::");
        base.push_str(pkg);
        base.push_str("::");
      }
      base.push_str(type_name);
    }
  }

  match t.array_spec {
    None => {}
    Some(ArraySpecifier::Static { size }) => {
      base = format!("[{base};{size}]");
    }
    Some(ArraySpecifier::Unbounded) | Some(ArraySpecifier::Bounded { .. }) => {
      base = format!("Vec<{base}>");
    }
  }

  Ok(base)
}

fn translate_value(v: &Value, expected_rust_type: &str) -> String {
  let float_cast = if expected_rust_type == "f32" || expected_rust_type == "f64" {
    expected_rust_type
  } else {
    ""
  };

  match v {
    Value::Bool(b) => {
      if *b {
        "true".to_string()
      } else {
        "false".to_string()
      }
    }
    Value::Float(f) => format!("{f}{float_cast}"),
    Value::Int(i) => format!("{i}{float_cast}"),
    Value::Uint(u) => format!("{u}{float_cast}"),
    Value::String(v) => String::from_utf8(v.to_vec()).unwrap(),
  }
}

#[cfg(test)]
mod tests {
  use pretty_assertions::assert_eq;

  use super::*;
  use crate::msggen::parser::msg_spec;

  /// Parse a `.msg` definition and generate the Rust code for it, so that test
  /// cases can be written as input/output text pairs.
  fn generate(name: &str, msg: &str) -> String {
    let (rest, lines) = msg_spec(msg).expect("Parse error");
    assert_eq!(rest, "", "Input was not parsed completely");
    let mut out = Vec::new();
    print_struct_definition(&mut out, name, &lines).expect("Generate error");
    String::from_utf8(out).expect("Generated code was not valid UTF-8")
  }

  #[test]
  fn simple_struct_test() {
    let msg = "\
float64 x
float64 y
";
    let expected = "\
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Point {
  pub x : f64,
  pub y : f64,
}
";
    assert_eq!(generate("Point", msg), expected);
  }

  #[test]
  fn comments_test() {
    let msg = "\
# Message comment
# second line

# Property comment
# line 2
float64 x
float64 y
";
    let expected = "\
/// Message comment
/// second line
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Point {
  /// Property comment
  /// line 2
  pub x : f64,
  pub y : f64,
}
";
    assert_eq!(generate("Point", msg), expected);
  }

  #[test]
  fn comments2_test() {
    let msg = "\
# Message comment
# second line

float64 x
float64 y
";
    let expected = "\
/// Message comment
/// second line
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Point {
  pub x : f64,
  pub y : f64,
}
";
    assert_eq!(generate("Point", msg), expected);
  }

  #[test]
  fn comments3_test() {
    let msg = "\
# Property comment
# line 2
float64 x
float64 y
";
    let expected = "\
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Point {
  /// Property comment
  /// line 2
  pub x : f64,
  pub y : f64,
}
";
    assert_eq!(generate("Point", msg), expected);
  }

  #[test]
  fn comments_trailing_test() {
    let msg = "\
float64 x #Commenting x
float64 y # Commenting y
";
    let expected = "\
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Point {
  ///Commenting x
  pub x : f64,
  /// Commenting y
  pub y : f64,
}
";
    assert_eq!(generate("Point", msg), expected);
  }

  #[test]
  fn comments_gap_inbetween_test() {
    let msg = "\
float64 x
#Random comment
# random part 2

# Commenting y
# comment y part 2
float64 y # Commenting y sameline
";
    let expected = "\
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Point {
  pub x : f64,
  //Random comment
  // random part 2
  /// Commenting y
  /// comment y part 2
  /// Commenting y sameline
  pub y : f64,
}
";
    assert_eq!(generate("Point", msg), expected);
  }

  #[test]
  fn comments_gap_inbetween2_test() {
    let msg = "\
int8 x=1
#Random comment
# random part 2

# Commenting y
# comment y part 2
int8 y=2 # Commenting y sameline
";
    let expected = "\
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Point {
}
impl Point {
  pub const x: i8 = 1;
  //Random comment
  // random part 2
  /// Commenting y
  /// comment y part 2
  /// Commenting y sameline
  pub const y: i8 = 2;
}
";
    assert_eq!(generate("Point", msg), expected);
  }

  #[test]
  fn constant_comments_test() {
    let msg = "\
int8 x=1 #Commenting x
int8 y=6 # Commenting y
";
    let expected = "\
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Point {
}
impl Point {
  ///Commenting x
  pub const x: i8 = 1;
  /// Commenting y
  pub const y: i8 = 6;
}
";
    assert_eq!(generate("Point", msg), expected);
  }

  #[test]
  fn constant_test() {
    let msg = "\
uint8 RESULT_OK=0
uint8 RESULT_FAILED=1
uint8 result
";
    let expected = "\
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Res {
  pub result : u8,
}
impl Res {
  pub const RESULT_OK: u8 = 0;
  pub const RESULT_FAILED: u8 = 1;
}
";
    assert_eq!(generate("Res", msg), expected);
  }

  #[test]
  fn constant_floats_test() {
    let msg = "\
float64 SOME=1.0
float64 OTHER=1
float64 PI=3.141592653589793
float64 result
";
    let expected = "\
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Res {
  pub result : f64,
}
impl Res {
  pub const SOME: f64 = 1f64;
  pub const OTHER: f64 = 1f64;
  pub const PI: f64 = 3.141592653589793f64;
}
";
    assert_eq!(generate("Res", msg), expected);
  }

  #[test]
  fn constant_inbetween_test() {
    let msg = "\
bool some
uint8 RESULT_OK=0
uint8 RESULT_OTHER=1
uint8 RESULT_FAILED=2
uint8 result
";
    let expected = "\
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Res {
  pub some : bool,
  pub result : u8,
}
impl Res {
  pub const RESULT_OK: u8 = 0;
  pub const RESULT_OTHER: u8 = 1;
  pub const RESULT_FAILED: u8 = 2;
}
";
    assert_eq!(generate("Res", msg), expected);
  }

  #[test]
  fn defaults_test() {
    let msg = "\
bool some true
uint8 result 7
float64 x 1
float64 y 2.0
";
    let expected = "\
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Res {
  pub some : bool,
  pub result : u8,
  pub x : f64,
  pub y : f64,
}
impl Default for Res {
  fn default() -> Self {
    Self {
      some: true,
      result: 7,
      x: 1f64,
      y: 2f64,
    }
  }
}
";
    assert_eq!(generate("Res", msg), expected);
  }
}
