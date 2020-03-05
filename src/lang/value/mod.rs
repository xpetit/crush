mod value_definition;
mod value_type;

use std::cmp::Ordering;
use std::hash::Hasher;
use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Local};
use regex::Regex;

use crate::{
    lang::command::Closure,
    util::file::cwd,
    lang::table::Table,
    lang::errors::{error, CrushError, to_crush_error},
    util::glob::Glob,
};
use crate::lang::{list::List, command::SimpleCommand, command::ConditionCommand, table::TableStream, dict::Dict, table::ColumnType, binary::BinaryReader, table::TableReader, list::ListReader, dict::DictReader, table::Row};
use crate::lang::errors::{CrushResult, argument_error};
use chrono::Duration;
use crate::util::time::duration_format;
use crate::lang::scope::Scope;
use crate::lang::r#struct::Struct;
use crate::lang::stream::{streams, Readable};
use std::io::{Read, Error};
use std::convert::TryFrom;

pub use value_type::ValueType;
pub use value_definition::ValueDefinition;

#[derive(Debug)]
pub enum Value {
    Text(Box<str>),
    Integer(i128),
    Time(DateTime<Local>),
    Duration(Duration),
    Field(Vec<Box<str>>),
    Glob(Glob),
    Regex(Box<str>, Regex),
    Command(SimpleCommand),
    Closure(Closure),
    ConditionCommand(ConditionCommand),
    TableStream(TableStream),
    File(Box<Path>),
    Table(Table),
    Struct(Struct),
    List(List),
    Dict(Dict),
    Scope(Scope),
    Bool(bool),
    Float(f64),
    Empty(),
    BinaryStream(Box<dyn BinaryReader>),
    Binary(Vec<u8>),
    Type(ValueType),
}

fn hex(v: u8) -> String {
    let arr = vec!["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "a", "b", "c", "d", "e", "f"];
    format!("{}{}", v>>4, v & 15)
}

impl Value {
    pub fn to_string(&self) -> String {
        return match self {
            Value::Text(val) => val.to_string(),
            Value::Integer(val) => val.to_string(),
            Value::Time(val) => val.format("%Y-%m-%d %H:%M:%S %z").to_string(),
            Value::Field(val) => format!(r"%{}", val.join(".")),
            Value::Glob(val) => format!("*{{{}}}", val.to_string()),
            Value::Regex(val, _) => format!("regex{{{}}}", val),
            Value::Command(_) => "Command".to_string(),
            Value::ConditionCommand(_) => "Command".to_string(),
            Value::File(val) => val.to_str().unwrap_or("<Broken file>").to_string(),
            Value::Table(_) => "<Rows>".to_string(),
            Value::Struct(row) => row.to_string(),
            Value::Closure(c) => c.to_string(),
            Value::TableStream(_) => "<Output>".to_string(),
            Value::List(l) => l.to_string(),
            Value::Duration(d) => duration_format(d),
            Value::Scope(env) => env.to_string(),
            Value::Bool(v) => (if *v { "true" } else { "false" }).to_string(),
            Value::Dict(d) => d.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Empty() => "<empty>".to_string(),
            Value::BinaryStream(_) => "<binary stream>".to_string(),
            Value::Binary(v) => v.iter().map(|u| hex(*u)).collect::<Vec<String>>().join(""),
            Value::Type(t) => t.to_string(),
        };
    }

    pub fn alignment(&self) -> Alignment {
        return match self {
            Value::Time(_) | Value::Duration(_) | Value::Integer(_) => Alignment::Right,
            _ => Alignment::Left,
        };
    }

    pub fn empty_stream() -> Value {
        let (_s, r) = streams(vec![]);
        Value::TableStream(TableStream { stream: r })
    }

    pub fn text(s: &str) -> Value {
        Value::Text(Box::from(s))
    }

    pub fn readable(&self) -> Option<Box<Readable>> {
        return match self {
            Value::TableStream(s) => Some(Box::from(s.stream.clone())),
            Value::Table(r) => Some(Box::from(TableReader::new(r.clone()))),
            Value::List(l) => Some(Box::from(ListReader::new(l.clone(), "value"))),
            Value::Dict(d) => Some(Box::from(DictReader::new(d.clone()))),
            _ => None,
        }
    }

    pub fn value_type(&self) -> ValueType {
        return match self {
            Value::Text(_) => ValueType::Text,
            Value::Integer(_) => ValueType::Integer,
            Value::Time(_) => ValueType::Time,
            Value::Field(_) => ValueType::Field,
            Value::Glob(_) => ValueType::Glob,
            Value::Regex(_, _) => ValueType::Regex,
            Value::Command(_) => ValueType::Command,
            Value::ConditionCommand(_) => ValueType::Command,
            Value::File(_) => ValueType::File,
            Value::TableStream(o) => ValueType::TableStream(o.stream.types().clone()),
            Value::Table(r) => ValueType::Table(r.types().clone()),
            Value::Struct(r) => ValueType::Struct(r.types().clone()),
            Value::Closure(_) => ValueType::Closure,
            Value::List(l) => l.list_type(),
            Value::Duration(_) => ValueType::Duration,
            Value::Scope(_) => ValueType::Scope,
            Value::Bool(_) => ValueType::Bool,
            Value::Dict(d) => d.dict_type(),
            Value::Float(_) => ValueType::Float,
            Value::Empty() => ValueType::Empty,
            Value::BinaryStream(_) => ValueType::BinaryStream,
            Value::Binary(_) => ValueType::Binary,
            Value::Type(_) => ValueType::Type,
        };
    }

    pub fn file_expand(&self, v: &mut Vec<Box<Path>>) -> CrushResult<()> {
        match self {
            Value::Text(s) => v.push(Box::from(Path::new(s.as_ref()))),
            Value::File(p) => v.push(p.clone()),
            Value::Glob(pattern) => pattern.glob_files(&cwd()?, v)?,
            Value::TableStream(s) => {
                let t = s.stream.types();
                if t.len() == 1 && t[0].cell_type == ValueType::File {
                    loop {
                        match s.stream.recv() {
                            Ok(row) => {
                                if let Value::File(f) = row.into_vec().remove(0) {
                                    v.push(f);
                                }
                            },
                            Err(_) => break,
                        }
                    }
                } else {
                    return argument_error("Table stream must contain one column of type file");
                }
            }
            _ => return error("Expected a file name"),
        }
        Ok(())
    }

    pub fn materialize(self) -> Value {
        match self {
            Value::TableStream(output) => {
                let mut rows = Vec::new();
                loop {
                    match output.stream.recv() {
                        Ok(r) => rows.push(r.materialize()),
                        Err(_) => break,
                    }
                }
                Value::Table(Table::new(ColumnType::materialize(output.stream.types()), rows ))
            }
            Value::BinaryStream(mut s) => {
                let mut vec = Vec::new();
                std::io::copy(s.as_mut(), &mut vec);
                Value::Binary(vec)
            }
            Value::Table(r) => Value::Table(r.materialize()),
            Value::Dict(d) => Value::Dict(d.materialize()),
            Value::Struct(r) => Value::Struct(r.materialize()),
            Value::List(l) => Value::List(l.materialize()),
            _ => self,
        }
    }

    pub fn cast(self, new_type: ValueType) -> CrushResult<Value> {
        if self.value_type() == new_type {
            return Ok(self);
        }

        /*
        This function is silly and overly large. Instead of mathcing on every source/destination pair, it should do
        two matches, one to convert any cell to a string, and one to convert a string to any cell. That would shorten
        this monstrosity to a sane size.
        */
        match (self, new_type) {
            (Value::Text(s), ValueType::File) => Ok(Value::File(Box::from(Path::new(s.as_ref())))),
            (Value::Text(s), ValueType::Glob) => Ok(Value::Glob(Glob::new(&s))),
            (Value::Text(s), ValueType::Integer) => to_crush_error(s.parse::<i128>()).map(|v| Value::Integer(v)),
            (Value::Text(s), ValueType::Field) => Ok(Value::Field(vec![s])),
            (Value::Text(s), ValueType::Regex) => to_crush_error(Regex::new(s.as_ref()).map(|v| Value::Regex(s, v))),
            (Value::Text(s), ValueType::Binary) => Ok(Value::Binary(s.bytes().collect())),
            (Value::Text(s), ValueType::Float) => Ok(Value::Float(to_crush_error(f64::from_str(&s))?)),

            (Value::File(s), ValueType::Text) => match s.to_str() {
                Some(s) => Ok(Value::Text(Box::from(s))),
                None => error("File name is not valid unicode")
            },
            (Value::File(s), ValueType::Glob) => match s.to_str() {
                Some(s) => Ok(Value::Glob(Glob::new(s))),
                None => error("File name is not valid unicode")
            },
            (Value::File(s), ValueType::Integer) => match s.to_str() {
                Some(s) => to_crush_error(s.parse::<i128>()).map(|v| Value::Integer(v)),
                None => error("File name is not valid unicode")
            },
            (Value::File(s), ValueType::Regex) => match s.to_str() {
                Some(s) => to_crush_error(Regex::new(s.as_ref()).map(|v| Value::Regex(Box::from(s), v))),
                None => error("File name is not valid unicode")
            },

            (Value::Glob(s), ValueType::Text) => Ok(Value::Text(s.to_string().clone().into_boxed_str())),
            (Value::Glob(s), ValueType::File) => Ok(Value::File(Box::from(Path::new(s.to_string().as_str())))),
            (Value::Glob(s), ValueType::Integer) => to_crush_error(s.to_string().parse::<i128>()).map(|v| Value::Integer(v)),
            (Value::Glob(g), ValueType::Regex) => {
                let s = g.to_string().as_str();
                to_crush_error(Regex::new(s).map(|v| Value::Regex(Box::from(s), v)))
            }
            /*
                        (Cell::Field(s), CellType::File) => Ok(Cell::File(Box::from(Path::new(s.as_ref())))),
                        (Cell::Field(s), CellType::Glob) => Ok(Cell::Glob(Glob::new(&s))),
                        (Cell::Field(s), CellType::Integer) => to_job_error(s.parse::<i128>()).map(|v| Cell::Integer(v)),
                        (Cell::Field(s), CellType::Text) => Ok(Cell::Text(s)),
                        (Cell::Field(s), CellType::Op) => Ok(Cell::Op(s)),
                        (Cell::Field(s), CellType::Regex) => to_job_error(Regex::new(s.as_ref()).map(|v| Cell::Regex(s, v))),
            */
            (Value::Regex(s, _), ValueType::File) => Ok(Value::File(Box::from(Path::new(s.as_ref())))),
            (Value::Regex(s, _), ValueType::Glob) => Ok(Value::Glob(Glob::new(&s))),
            (Value::Regex(s, _), ValueType::Integer) => to_crush_error(s.parse::<i128>()).map(|v| Value::Integer(v)),
            (Value::Regex(s, _), ValueType::Text) => Ok(Value::Text(s)),

            (Value::Integer(i), ValueType::Text) => Ok(Value::Text(i.to_string().into_boxed_str())),
            (Value::Integer(i), ValueType::File) => Ok(Value::File(Box::from(Path::new(i.to_string().as_str())))),
            (Value::Integer(i), ValueType::Glob) => Ok(Value::Glob(Glob::new(i.to_string().as_str()))),
            (Value::Integer(i), ValueType::Field) => Ok(Value::Field(vec![i.to_string().into_boxed_str()])),
            (Value::Integer(i), ValueType::Regex) => {
                let s = i.to_string();
                to_crush_error(Regex::new(s.as_str()).map(|v| Value::Regex(s.into_boxed_str(), v)))
            }
            (Value::Integer(i), ValueType::Float) => Ok(Value::Float(i as f64)),

            (Value::Type(s), ValueType::Text) => Ok(Value::Text(Box::from(s.to_string()))),

            (Value::Float(i), ValueType::Integer) => Ok(Value::Integer(i as i128)),
            (Value::Float(i), ValueType::Text) => Ok(Value::Text(i.to_string().into_boxed_str())),

            (Value::Binary(s), ValueType::Text) => Ok(Value::Text(to_crush_error(String::from_utf8(s))?.into_boxed_str())),

            (Value::BinaryStream(mut s), ValueType::Text) => {
                let mut v = Vec::new();
                s.read_to_end(&mut v);
                Ok(Value::Text(to_crush_error(String::from_utf8(v))?.into_boxed_str()))
            },

            (Value::TableStream(s), ValueType::List(t)) => {
                if s.stream.types().len()!=1 {
                    return error("Stream must have exactly one element to convert to list");
                }
                if s.stream.types()[0].cell_type != t.as_ref().clone() {
                    return error(format!("Incompatible stream type, {} vs {}", s.stream.types()[0].cell_type.to_string(), t.to_string()).as_str());
                }
                let mut v = Vec::new();
                loop {
                    match s.stream.recv() {
                        Ok(r) => v.push(r.into_vec().remove(0)),
                        Err(_) => break,
                    }
                }
                Ok(Value::List(List::new(t.as_ref().clone(), v)))
            }

            _ => error("Unimplemented conversion"),
        }
    }
}

impl Clone for Value {
    fn clone(&self) -> Self {
        match self {
            Value::Text(v) => Value::Text(v.clone()),
            Value::Integer(v) => Value::Integer(v.clone()),
            Value::Time(v) => Value::Time(v.clone()),
            Value::Field(v) => Value::Field(v.clone()),
            Value::Glob(v) => Value::Glob(v.clone()),
            Value::Regex(v, r) => Value::Regex(v.clone(), r.clone()),
            Value::Command(v) => Value::Command(v.clone()),
            Value::ConditionCommand(v) => Value::ConditionCommand(v.clone()),
            Value::File(v) => Value::File(v.clone()),
            Value::Table(r) => Value::Table(r.clone()),
            Value::Struct(r) => Value::Struct(r.clone()),
            Value::Closure(c) => Value::Closure(c.clone()),
            Value::TableStream(s) => Value::TableStream(s.clone()),
            Value::List(l) => Value::List(l.clone()),
            Value::Duration(d) => Value::Duration(d.clone()),
            Value::Scope(e) => Value::Scope(e.clone()),
            Value::Bool(v) => Value::Bool(v.clone()),
            Value::Dict(d) => Value::Dict(d.clone()),
            Value::Float(f) => Value::Float(f.clone()),
            Value::Empty() => Value::Empty(),
            Value::BinaryStream(v) => Value::BinaryStream(v.as_ref().clone()),
            Value::Binary(v) => Value::Binary(v.clone()),
            Value::Type(t) => Value::Type(t.clone()),
        }
    }
}

impl std::hash::Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if !self.value_type().is_hashable() {
            panic!("Can't hash mutable cell types!");
        }
        match self {
            Value::Text(v) => v.hash(state),
            Value::Integer(v) => v.hash(state),
            Value::Time(v) => v.hash(state),
            Value::Field(v) => v.hash(state),
            Value::Glob(v) => v.hash(state),
            Value::Regex(v, _) => v.hash(state),
            Value::Command(_) => {}
            Value::ConditionCommand(_) => {}
            Value::File(v) => v.hash(state),
            Value::Duration(d) => d.hash(state),
            Value::Bool(v) => v.hash(state),
            Value::Binary(v) => v.hash(state),

            Value::Scope(_) | Value::Dict(_) | Value::Table(_) | Value::Closure(_) |
            Value::List(_) | Value::TableStream(_) | Value::Struct(_) | Value::Float(_)
            | Value::BinaryStream(_) => panic!("Can't hash output"),
            Value::Empty() => {}
            Value::Type(v) => v.to_string().hash(state),
        }
    }
}

fn file_result_compare(f1: &Path, f2: &Path) -> bool {
    match (f1.canonicalize(), f2.canonicalize()) {
        (Ok(p1), Ok(p2)) => p1 == p2,
        _ => false,
    }
}

impl std::cmp::PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        return match (self, other) {
            (Value::Text(val1), Value::Text(val2)) => val1 == val2,
            (Value::Glob(glb), Value::Text(val)) => glb.matches(val),
            (Value::Text(val), Value::Glob(glb)) => glb.matches(val),
            (Value::Integer(val1), Value::Integer(val2)) => val1 == val2,
            (Value::Time(val1), Value::Time(val2)) => val1 == val2,
            (Value::Duration(val1), Value::Duration(val2)) => val1 == val2,
            (Value::Field(val1), Value::Field(val2)) => val1 == val2,
            (Value::Glob(val1), Value::Glob(val2)) => val1 == val2,
            (Value::Regex(val1, _), Value::Regex(val2, _)) => val1 == val2,
            (Value::Command(val1), Value::Command(val2)) => val1 == val2,
            (Value::List(val1), Value::List(val2)) => val1 == val2,
            (Value::Table(val1), Value::Table(val2)) => match val1.partial_cmp(val2) {
                None => false,
                Some(o) => o == Ordering::Equal,
            },
            (Value::Struct(val1), Value::Struct(val2)) => match val1.partial_cmp(val2) {
                None => false,
                Some(o) => o == Ordering::Equal,
            },
            (Value::File(val1), Value::File(val2)) => file_result_compare(val1.as_ref(), val2.as_ref()),
            (Value::Text(val1), Value::File(val2)) => file_result_compare(&Path::new(&val1.to_string()), val2.as_ref()),
            (Value::File(val1), Value::Text(val2)) => file_result_compare(&Path::new(&val2.to_string()), val1.as_ref()),
            (Value::Bool(val1), Value::Bool(val2)) => val1 == val2,
            _ => false,
        };
    }
}

pub enum Alignment {
    Left,
    Right,
}

impl std::cmp::PartialOrd for Value {
    fn partial_cmp(&self, other: &Value) -> Option<Ordering> {
        let t1 = self.value_type();
        let t2 = other.value_type();
        if t1 != t2 {
            return Some(t1.cmp(&t2));
        }
        return match (self, other) {
            (Value::Text(val1), Value::Text(val2)) => Some(val1.cmp(val2)),
            (Value::Field(val1), Value::Field(val2)) => Some(val1.cmp(val2)),
            (Value::Glob(val1), Value::Glob(val2)) => Some(val1.cmp(val2)),
            (Value::Regex(val1, _), Value::Regex(val2, _)) => Some(val1.cmp(val2)),
            (Value::Integer(val1), Value::Integer(val2)) => Some(val1.cmp(val2)),
            (Value::Time(val1), Value::Time(val2)) => Some(val1.cmp(val2)),
            (Value::File(val1), Value::File(val2)) => Some(val1.cmp(val2)),
            (Value::Duration(val1), Value::Duration(val2)) => Some(val1.cmp(val2)),
            (Value::Command(_), Value::Command(_)) => None,
            (Value::Closure(_), _) => None,
            (Value::TableStream(_), _) => None,
            (Value::Table(val1), Value::Table(val2)) => val1.partial_cmp(val2),
            (Value::Struct(val1), Value::Struct(val2)) => val1.partial_cmp(val2),
            (Value::List(val1), Value::List(val2)) => val1.partial_cmp(val2),
            (Value::Bool(val1), Value::Bool(val2)) => Some(val1.cmp(val2)),
            _ => None,
        };
    }
}

impl std::cmp::Eq for Value {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_casts() {
        assert_eq!(Value::Text(Box::from("112432")).cast(ValueType::Integer).is_err(), false);
        assert_eq!(Value::text("1d").cast(ValueType::Integer).is_err(), true);
        assert_eq!(Value::text("1d").cast(ValueType::Glob).is_err(), false);
        assert_eq!(Value::text("1d").cast(ValueType::File).is_err(), false);
        assert_eq!(Value::text("1d").cast(ValueType::Time).is_err(), true);
        assert_eq!(Value::text("fad").cast(ValueType::Field).is_err(), false);
    }

    #[test]
    fn test_duration_format() {
        assert_eq!(duration_format(&Duration::microseconds(0)), "0".to_string());
        assert_eq!(duration_format(&Duration::microseconds(1)), "0.000001".to_string());
        assert_eq!(duration_format(&Duration::microseconds(100)), "0.0001".to_string());
        assert_eq!(duration_format(&Duration::milliseconds(1)), "0.001".to_string());
        assert_eq!(duration_format(&Duration::milliseconds(1000)), "1".to_string());
        assert_eq!(duration_format(&Duration::milliseconds(1000 * 61)), "1:01".to_string());
        assert_eq!(duration_format(&Duration::milliseconds(1000 * 3601)), "1:00:01".to_string());
        assert_eq!(duration_format(&Duration::milliseconds(1000 * (3600 * 24 * 3 + 1))), "3d0:00:01".to_string());
        assert_eq!(duration_format(&Duration::milliseconds(1000 * (3600 * 24 * 365 * 10 + 1))), "10y0d0:00:01".to_string());
        assert_eq!(duration_format(&Duration::milliseconds(1000 * (3600 * 24 * 365 * 10 + 1) + 1)), "10y0d0:00:01".to_string());
    }
}
