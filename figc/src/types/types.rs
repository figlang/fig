use wasm_encoder::ValType;

use crate::codegen::codegen::CompilerError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    I8,

    I32,
    I64,

    F32,
    F64,

    Char,

    Bool,

    Array(Box<Type>),

    Ref(Box<Type>),

    Custom(String),
}

impl From<String> for Type {
    fn from(value: String) -> Self {
        let splited = value.split("[");

        if splited.clone().into_iter().count() >= 2 {
            // This means its an array
            let ty = Type::from(splited.into_iter().nth(0).unwrap().to_string());

            return Self::Array(Box::from(ty));
        }

        // Refrence check
        if value.chars().into_iter().nth(0).unwrap() == '&' {
            let ty = Type::from(value.chars().into_iter().take(1).collect::<String>());

            return Self::Ref(Box::from(ty));
        }

        match value.as_str() {
            "i8" => Self::I8,
            "i32" => Self::I32,
            "i64" => Self::I64,
            "f32" => Self::F32,
            "f64" => Self::F64,
            "char" => Self::Char,
            "bool" => Self::Bool,
            other => Self::Custom(other.to_string()),
        }
    }
}

pub fn types_to_val_types(types: Vec<Type>) -> Result<Vec<ValType>, CompilerError> {
    let mut result = vec![];

    for ty in types {
        result.push(ty.try_into()?);
    }

    Ok(result)
}

impl TryInto<ValType> for Type {
    type Error = CompilerError;

    fn try_into(self) -> Result<ValType, Self::Error> {
        match self {
            Self::I8 => Ok(ValType::I32),
            Self::I32 => Ok(ValType::I32),
            Self::I64 => Ok(ValType::I64),
            Self::F32 => Ok(ValType::F32),
            Self::F64 => Ok(ValType::F64),

            // Its 0 or 1
            //
            // TODO: Check if we can have a type with 1 bit ?
            Self::Bool => Ok(ValType::I32),

            // pointer of the string
            Self::Char => Ok(ValType::I32),

            Self::Array(_) => Ok(ValType::I32),
            Self::Ref(_) => Ok(ValType::I32),

            // TODO: is this ok ?
            Self::Custom(_) => Ok(ValType::I32),
        }
    }
}
