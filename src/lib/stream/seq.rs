use crate::lang::command::ExecutionContext;
use crate::lang::errors::{CrushResult, error};
use crate::{
    lang::{
        table::Row,
        value::ValueType,
        value::Value
    }
};
use crate::lang::{table::ColumnType, argument::Argument, table::TableReader};
use crate::lib::command_util::find_field_from_str;
use crate::lang::stream::{Readable};
use crate::lib::parse_util::single_argument_integer;

pub fn perform(mut context: ExecutionContext) -> CrushResult<()> {
    let c  =single_argument_integer(context.arguments)?;
    let output = context.output.initialize(vec![
        ColumnType::named("value", ValueType::Integer)])?;

    for i in 0..c {
        output.send(Row::new(vec![Value::Integer(i)]))?;
    }
    Ok(())
}
