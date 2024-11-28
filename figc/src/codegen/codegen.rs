// MIT License
//
// Copyright (c) 2024 The Fig Programming Language
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use std::{
    collections::{HashMap, VecDeque},
    fmt::Display,
};

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataSection, ElementSection, EntityType, ExportKind,
    ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection,
    Instruction, MemArg, MemorySection, MemoryType, Module, TableSection, TypeSection, ValType,
};

use crate::{
    lexer::token::Token,
    parser::ast::{
        ArrayExpr, BlockStatement, BooleanExpr, BreakStatement, BuiltinStatement, CallExpr,
        CharExpr, ConstStatement, DeRef, ExportStatement, Expression, ExternalStatement,
        FunctionMeta, FunctionStatement, Identifier, IfExpr, IndexExpr, InfixExpr, Integer,
        LetStatement, LoopStatement, ObjectAccess, ObjectExpr, PrefixExpr, Program, RefValue,
        ReturnStatement, SetStatement, Statement, StringExpr, StructStatement,
    },
    types::types::{types_to_val_types, Type},
};

use super::builtins::malloc;

#[derive(Debug)]
pub enum CompilerError {
    NotDefined(String),
    NotSupported(String),
    Type(String),
}

impl CompilerError {
    pub fn not_defined<'a>(what: &'a str, name: &'a str, at_line: u32) -> Self {
        Self::NotDefined(format!(
            "{} with name {} not defined at line {}",
            what, name, at_line
        ))
    }

    pub fn not_supported_expr<'a>(expr: &'a str, statement: &'a str, at_line: u32) -> Self {
        Self::NotDefined(format!(
            "Expression {} is not supported in {} statement at line {}",
            expr, statement, at_line
        ))
    }
}

impl Display for CompilerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompilerError::NotDefined(msg) => write!(f, "{}", msg),
            CompilerError::NotSupported(msg) => write!(f, "{}", msg),
            CompilerError::Type(msg) => write!(f, "{}", msg),
        }
    }
}

type CResult<T> = Result<T, CompilerError>;

pub trait WasmTypes {
    type Output;
    fn types(&self) -> Self::Output
    where
        Self: Sized;
}

impl WasmTypes for FunctionMeta {
    type Output = CResult<(Vec<ValType>, Vec<ValType>)>;

    fn types(&self) -> Self::Output {
        let mut param_type: Vec<ValType> = vec![];

        for param in &self.params {
            param_type.push(param.1.clone().try_into()?);
        }

        let return_type: Vec<ValType> = match self.return_type.clone() {
            Some(ret_type) => vec![ret_type.try_into()?],

            None => vec![],
        };

        Ok((param_type, return_type))
    }
}

pub trait Instructions<'a> {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>>
    where
        Self: Sized;
}

impl<'a> Instructions<'a> for CallExpr {
    // TODO: First check if the function exists
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];

        for arg in &self.arguments {
            result.extend(arg.generate_instructions(ctx)?);
        }

        let (func_id, return_type) = match ctx.function_ctx.get_function(&self.function.value) {
            Some(func) => Ok((func.id, func.return_type.clone())),
            None => Err(CompilerError::NotDefined(format!(
                "Function with name {} is not defined!",
                self.function.value
            ))),
        }?;

        if let Some(return_ty) = return_type {
            ctx.type_ctx.set_active_type(return_ty);
        }

        result.push(Instruction::Call(func_id));

        Ok(result)
    }
}

impl StringExpr {
    pub fn allocate(&self, ctx: &mut Context) -> i32 {
        // the + 1 is for \0 at the end of the string
        let size = self.string.len() + 1;

        let mut string = self.string.to_owned();
        string.push('\0');

        let current_mem_offset = ctx.memory_ctx.offset;
        ctx.global_ctx.set_global(
            "mem_offset",
            ConstExpr::i32_const(current_mem_offset + size as i32),
        );

        let ptr = ctx
            .memory_ctx
            .alloc_data(size as i32, string.as_bytes().to_vec());

        ptr
    }
}

impl<'a> Instructions<'a> for StringExpr {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let ptr = self.allocate(ctx);

        ctx.type_ctx
            .set_active_type(Type::Array(Box::new(Type::Char)));

        Ok(vec![Instruction::I32Const(ptr)])
    }
}

impl<'a> Instructions<'a> for CharExpr {
    fn generate_instructions(&self, _ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        // TODO: Is this Ok ?
        Ok(vec![Instruction::I32Const(self.ch as i32)])
    }
}

impl<'a> Instructions<'a> for RefValue {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        match self.value.as_ref() {
            Expression::Identifier(ident) => {
                let Some(local) = ctx.local_ctx.get_local_index(&ident.value) else {
                    return Err(CompilerError::NotDefined(format!(
                        "Variable with name {} is not defined!",
                        &ident.value
                    )));
                };

                Ok(vec![Instruction::LocalGet(local.index)])
            }

            Expression::Index(index) => Ok(index.get_offset(ctx)?),

            _ => panic!(),
        }
    }
}

impl<'a> Instructions<'a> for DeRef {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result = vec![];

        result.extend(self.value.generate_instructions(ctx)?);

        match ctx.type_ctx.active_type.clone() {
            Type::Array(arr_type) => match *arr_type.clone() {
                Type::Char | Type::I8 => {
                    // result.push(Instruction::I32Load8S(MemArg {
                    //     offset: 0,
                    //     align: 0,
                    //     memory_index: 0,
                    // }));
                }

                _ => {
                    // result.push(Instruction::I32Load(MemArg {
                    //     offset: 0,
                    //     align: 0,
                    //     memory_index: 0,
                    // }));
                }
            },

            _ => {
                result.push(Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                }));
            }
        };

        Ok(result)
    }
}

impl<'a> Instructions<'a> for Token {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        match self {
            Token::Plus => Ok(vec![match ctx.type_ctx.active_type {
                Type::I32 => Instruction::I32Add,
                Type::I64 => Instruction::I64Add,
                Type::F32 => Instruction::F32Add,
                Type::F64 => Instruction::F64Add,

                _ => Instruction::I32Add,
            }]),
            Token::Minus => Ok(vec![match ctx.type_ctx.active_type {
                Type::I32 => Instruction::I32Sub,
                Type::I64 => Instruction::I64Sub,
                Type::F32 => Instruction::F32Sub,
                Type::F64 => Instruction::F64Sub,

                _ => Instruction::I32Sub,
            }]),
            Token::ForwardSlash => Ok(vec![match ctx.type_ctx.active_type {
                Type::I32 => Instruction::I32DivS,
                Type::I64 => Instruction::I64DivS,
                Type::F32 => Instruction::F32Div,
                Type::F64 => Instruction::F64Div,

                _ => Instruction::I32DivS,
            }]),
            Token::Asterisk => Ok(vec![match ctx.type_ctx.active_type {
                Type::I32 => Instruction::I32Mul,
                Type::I64 => Instruction::I64Mul,
                Type::F32 => Instruction::F32Mul,
                Type::F64 => Instruction::F64Mul,

                _ => Instruction::I32Mul,
            }]),
            Token::Equal => Ok(vec![match ctx.type_ctx.active_type {
                Type::I32 => Instruction::I32Eq,
                Type::I64 => Instruction::I64Eq,
                Type::F32 => Instruction::F32Eq,
                Type::F64 => Instruction::F64Eq,

                _ => Instruction::I32Eq,
            }]),
            Token::NotEqual => Ok(vec![match ctx.type_ctx.active_type {
                Type::I32 => Instruction::I32Ne,
                Type::I64 => Instruction::I64Ne,
                Type::F32 => Instruction::F32Ne,
                Type::F64 => Instruction::F64Ne,

                _ => Instruction::I32Ne,
            }]),
            Token::LessThan => Ok(vec![match ctx.type_ctx.active_type {
                Type::I32 => Instruction::I32LeS,
                Type::I64 => Instruction::I64LeS,
                Type::F32 => Instruction::F32Le,
                Type::F64 => Instruction::F64Le,

                _ => Instruction::I32LeS,
            }]),
            Token::GreaterThan => Ok(vec![match ctx.type_ctx.active_type {
                Type::I32 => Instruction::I32GtS,
                Type::I64 => Instruction::I64GtS,
                Type::F32 => Instruction::F32Gt,
                Type::F64 => Instruction::F64Gt,

                _ => Instruction::I32GtS,
            }]),
            Token::Mod => Ok(vec![match ctx.type_ctx.active_type {
                Type::I32 => Instruction::I32RemS,
                Type::I64 => Instruction::I64RemS,
                Type::F32 => panic!("Can't REM f32 type"),
                Type::F64 => panic!("Can't REM f64 type"),

                _ => Instruction::I32RemS,
            }]),

            _ => todo!(),
        }
    }
}

impl<'a> Instructions<'a> for BooleanExpr {
    fn generate_instructions(&self, _ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        match self.value {
            true => Ok(vec![Instruction::I32Const(1)]),
            false => Ok(vec![Instruction::I32Const(0)]),
        }
    }
}

impl<'a> Instructions<'a> for PrefixExpr {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];

        match self.operator {
            Token::Minus => {
                ctx.type_ctx.set_active_type(Type::F32);
                let right_side = self.right.generate_instructions(ctx)?;

                result.extend(right_side);
                result.push(Instruction::F32Neg);
            }

            _ => panic!("Just - operation allowed"),
        }

        Ok(result)
    }
}

impl<'a> Instructions<'a> for InfixExpr {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];
        let left_side = self.left.generate_instructions(ctx)?;
        let left_side_type = ctx.type_ctx.active_type.clone();
        let operation = self.operator.generate_instructions(ctx)?;
        let right_side = self.right.generate_instructions(ctx)?;
        let right_side_type = ctx.type_ctx.active_type.clone();

        if left_side_type != right_side_type {
            return Err(CompilerError::Type(format!(
                "Left hand side {:?} is not equal to right hand side {:?}.",
                left_side_type, right_side_type
            )));
        }

        result.extend(left_side);
        result.extend(right_side);
        result.extend(operation);

        Ok(result)
    }
}

impl<'a> Instructions<'a> for Integer {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let const_instruction = match ctx.type_ctx.active_type {
            Type::I32 => Instruction::I32Const(self.value),
            Type::I64 => Instruction::I64Const(self.value as i64),
            Type::F32 => Instruction::F32Const(self.value as f32),
            Type::F64 => Instruction::F64Const(self.value as f64),

            _ => Instruction::I32Const(self.value),
        };

        Ok(vec![const_instruction])
    }
}

impl<'a> Instructions<'a> for ExternalStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        for func in &self.body.function_types {
            let (param_types, result_type) = func.types()?;

            let type_id = ctx
                .type_ctx
                .new_function_type(param_types.clone(), result_type);

            let params = param_types
                .into_iter()
                .zip(func.params.clone())
                .enumerate()
                .map(|(i, (t, param))| FunctionParam {
                    id: i as u32,
                    param_type: t,
                    name: param.0.value,
                })
                .collect::<Vec<FunctionParam>>();

            ctx.function_ctx.new_external_function(
                func.name.value.clone(),
                params,
                func.return_type.clone(),
            );

            ctx.import_ctx.import_func(
                &self.module.value,
                &func.name.value,
                EntityType::Function(type_id),
            );
        }
        Ok(vec![])
    }
}

impl<'a> Instructions<'a> for BuiltinStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        match self.function_meta.name.value.as_str() {
            "salloc" => {
                let type_index = ctx
                    .type_ctx
                    .new_function_type(vec![ValType::I32], vec![ValType::I32]);

                ctx.function_ctx.new_function(
                    type_index,
                    "salloc".to_string(),
                    vec![],
                    Some(Type::Array(Box::new(Type::Char))),
                );

                ctx.code_ctx.add_local(ValType::I32);
                ctx.code_ctx.new_function_code(malloc(), "salloc".into());

                // TODO: Add this to export generate_instructions functions
                ctx.export_ctx
                    .export_function(&"salloc".to_owned(), type_index);
            }
            "malloc" => {
                let type_index = ctx
                    .type_ctx
                    .new_function_type(vec![ValType::I32], vec![ValType::I32]);

                ctx.function_ctx.new_function(
                    type_index,
                    "malloc".to_string(),
                    vec![],
                    Some(Type::Array(Box::new(Type::I32))),
                );

                ctx.code_ctx.add_local(ValType::I32);
                ctx.code_ctx.new_function_code(malloc(), "malloc".into());

                // TODO: Add this to export generate_instructions functions
                ctx.export_ctx
                    .export_function(&"malloc".to_owned(), type_index);
            }

            "free" => {
                panic!("free(size: i32); Not working well !");
                //let type_index = ctx.type_ctx.new_function_type(vec![ValType::I32], vec![]);

                //ctx.function_ctx
                //    .new_function(type_index, "free".to_string(), vec![]);

                //ctx.code_ctx.new_function_code(free(), "free".into());
            }

            _ => todo!(),
        }

        Ok(vec![])
    }
}

impl<'a> Instructions<'a> for LoopStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];
        result.push(Instruction::Block(BlockType::Empty));

        result.push(Instruction::Loop(BlockType::Empty));

        let block = self.block.generate_instructions(ctx)?;
        result.extend(block);

        result.push(Instruction::Br(0));
        result.push(Instruction::End);
        result.push(Instruction::End);

        Ok(result)
    }
}

impl<'a> Instructions<'a> for BreakStatement {
    fn generate_instructions(&self, _ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        // TODO: not fix value
        // need a new ctx :}
        Ok(vec![Instruction::Br(2)])
    }
}

impl IndexExpr {
    fn get_instruction<'a>(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut offset = self.get_offset(ctx)?;

        // Get the variable type
        let variable = ctx.local_ctx.get_local_type(&self.variable.value).unwrap();

        ctx.type_ctx.set_active_type(variable.clone());

        match variable {
            Type::Array(arr_type) => match *arr_type.clone() {
                Type::Char | Type::I8 => {
                    offset.push(Instruction::I32Load8S(MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }));
                }
                _ => {
                    offset.push(Instruction::I32Load(MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }));
                }
            },

            _ => {
                panic!();
            }
        }

        Ok(offset)
    }

    fn get_offset<'a>(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];
        // First we want to get the offset,
        // we add the current offset with the self.index
        let Some(variable) = ctx.local_ctx.get_local_index(&self.variable.value) else {
            return Err(CompilerError::NotDefined(format!(
                "Variable with name {} is not defined!",
                self.variable.value
            )));
        };

        let variable_type = ctx.local_ctx.get_local_type(&self.variable.value).unwrap();
        if !matches!(variable_type, Type::Array(_)) {
            return Err(CompilerError::NotSupported(format!(
                "Variable with name {} is not an array!",
                self.variable.value,
            )));
        }

        // Is this good solution ? //FIX
        result.push(Instruction::LocalGet(variable.index));

        if variable.derefable {
            result.push(Instruction::I32Load(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
        }

        result.extend(self.index.generate_instructions(ctx)?);
        result.push(Instruction::I32Add);

        Ok(result)
    }
}

impl<'a> Instructions<'a> for IndexExpr {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        Ok(vec![match ctx.type_ctx.active_type.clone() {
            Type::I8 | Type::Char => Instruction::I32Store8(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }),

            _ => Instruction::I32Store(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }),
        }])
    }
}

impl<'a> Instructions<'a> for SetStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];
        let expression = self.expression.generate_instructions(ctx)?;

        match &self.variable {
            Expression::Index(index_expr) => {
                result.extend(index_expr.get_offset(ctx)?);
                result.extend(expression);
                result.extend(index_expr.generate_instructions(ctx)?);
            }

            Expression::Identifier(ident) => {
                let Some(local) = ctx.local_ctx.get_local_index(&ident.value) else {
                    return Err(CompilerError::NotDefined(format!(
                        "Variable with name {} is not defined!",
                        ident.value
                    )));
                };

                result.push(Instruction::LocalGet(local.index));
                result.extend(expression);

                result.push(Instruction::I32Store(MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                }));

                result.push(Instruction::LocalGet(local.index));

                result.push(Instruction::LocalSet(local.index));
            }

            // TODO: this does't works
            Expression::ObjectAccess(acc) => {
                // Get the variable
                let Some(var_type) = ctx.local_ctx.get_local_type(&acc.variable) else {
                    return Err(CompilerError::NotDefined(format!(
                        "Variable with name {} is not defined!",
                        &acc.variable
                    )));
                };

                let Some(local) = ctx.local_ctx.get_local_index(&acc.variable) else {
                    return Err(CompilerError::NotDefined(format!(
                        "Variable with name {} is not defined!",
                        acc.variable
                    )));
                };

                match var_type {
                    Type::Custom(struct_name) => {
                        let Some(structure) = ctx.type_ctx.get_struct(struct_name) else {
                            return Err(CompilerError::NotDefined(format!(
                                "Structure with name {} is not defined!",
                                &acc.variable
                            )));
                        };

                        let fields = acc.fields.clone();
                        let mut next_field = Some(Box::new(acc.fields.clone()));
                        let mut ty = ctx.type_ctx.active_type.clone();

                        while let Some(field) = next_field.clone() {
                            let Some(structure_field) = structure.fields.get(&fields.field) else {
                                return Err(CompilerError::NotDefined(format!(
                                    "{} structure does't have field named {}!",
                                    structure.name, field.field
                                )));
                            };

                            ty = structure_field.1.clone();

                            // Now get the property ( by index )
                            result.push(Instruction::LocalGet(local.index));
                            result.push(Instruction::I32Const(structure_field.0));
                            result.push(Instruction::I32Add);

                            next_field = field.inner_field.clone();

                            if next_field.is_some() {
                                result.push(Instruction::I32Load(MemArg {
                                    offset: 0,
                                    align: 0,
                                    memory_index: 0,
                                }));
                            }
                        }

                        result.extend(expression);

                        result.push(Instruction::I32Store(MemArg {
                            offset: 0,
                            align: 0,
                            memory_index: 0,
                        }));

                        ctx.type_ctx.set_active_type(ty);
                    }

                    t => {
                        return Err(CompilerError::NotSupported(format!(
                            "You can't access field of non object type! {:?}",
                            t
                        )));
                    }
                }
            }

            _ => unreachable!(),
        }

        Ok(result)
    }
}

impl<'a> Instructions<'a> for ExportStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        match *self.value {
            Statement::Function(ref _f) => {
                let instructions = self.value.generate_instructions(ctx)?;

                let Some(current_function) = ctx.function_ctx.current_function() else {
                    return Err(CompilerError::NotDefined(
                        "Function not defined!".to_string(),
                    ));
                };

                ctx.export_ctx
                    .export_function(&current_function.name, current_function.id);

                Ok(instructions)
            }

            Statement::Const(ref cnst) => {
                let instructions = self.value.generate_instructions(ctx)?;

                let Some(glob) = ctx.global_ctx.get_global(&cnst.name.value) else {
                    return Err(CompilerError::NotDefined("Const not defined!".to_string()));
                };

                ctx.export_ctx.export_global(&cnst.name.value, glob.0);

                Ok(instructions)
            }

            _ => panic!("You can't export statements other than function and const"),
        }
    }
}

impl<'a> Instructions<'a> for ReturnStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result = vec![];

        if let Some(func) = ctx.function_ctx.current_function() {
            ctx.type_ctx.set_active_type(
                func.return_type
                    .unwrap_or_else(|| panic!("This Function doesn't return any value!")),
            );
        }

        let expr = self.return_value.generate_instructions(ctx)?;
        result.extend(expr);

        result.push(Instruction::Return);

        Ok(result)
    }
}

impl<'a> Instructions<'a> for Expression {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        match self {
            Expression::Integer(int) => Ok(int.generate_instructions(ctx)?),
            Expression::Infix(infix) => Ok(infix.generate_instructions(ctx)?),
            Expression::Prefix(prefix) => Ok(prefix.generate_instructions(ctx)?),
            Expression::Identifier(ident) => Ok(ident.generate_instructions(ctx)?),
            Expression::Call(call) => Ok(call.generate_instructions(ctx)?),
            Expression::String(s) => Ok(s.generate_instructions(ctx)?),
            Expression::Char(c) => Ok(c.generate_instructions(ctx)?),
            Expression::If(if_expr) => Ok(if_expr.generate_instructions(ctx)?),
            Expression::Boolean(bool_expr) => Ok(bool_expr.generate_instructions(ctx)?),
            Expression::Index(index_expr) => Ok(index_expr.get_instruction(ctx)?),
            Expression::Ref(ref_expr) => Ok(ref_expr.generate_instructions(ctx)?),
            Expression::DeRef(deref_expr) => Ok(deref_expr.generate_instructions(ctx)?),
            Expression::Object(obj_expr) => Ok(obj_expr.generate_instructions(ctx)?),
            Expression::ObjectAccess(obj_access) => Ok(obj_access.generate_instructions(ctx)?),
            Expression::Array(array_expr) => Ok(array_expr.generate_instructions(ctx)?),
        }
    }
}

impl<'a> Instructions<'a> for Identifier {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let local = ctx.local_ctx.get_local_index(&self.value);
        match local {
            Some(local) => {
                if local.derefable {
                    return Ok(vec![
                        Instruction::LocalGet(local.index),
                        Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 0,
                            memory_index: 0,
                        }),
                    ]);
                } else {
                    return Ok(vec![Instruction::LocalGet(local.index)]);
                }
            }

            None => match ctx.global_ctx.get_global(&self.value) {
                Some(glob) => {
                    // TODO
                    //                ctx.type_ctx
                    //                    .set_active_type(ctx.local_ctx.get_local_type(&self.value).unwrap().clone());
                    //
                    Ok(vec![Instruction::GlobalGet(glob.0.clone())])
                }
                None => Err(CompilerError::NotDefined(format!(
                    "Variable with name {} is not defined!",
                    self.value
                ))),
            },
        }
    }
}

impl<'a> Instructions<'a> for LetStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        // create the local and set the active local

        let mut result: Vec<Instruction> = vec![];

        if let Some(ty) = self.value_type.clone() {
            ctx.type_ctx.set_active_type(ty);
        }

        let let_value = self.value.generate_instructions(ctx)?;

        let let_type = if let Some(ty) = self.value_type.clone() {
            ty
        } else {
            ctx.type_ctx.active_type.clone()
        };

        //if let_type != ctx.type_ctx.active_type {
        //    return Err(CompilerError::Type(format!(
        //        "The let type {:?} is not equal to value type {:?}",
        //        let_type, ctx.type_ctx.active_type
        //    )));
        //}

        let local_index = ctx
            .local_ctx
            .new_local(self.name.value.clone(), let_type.clone(), true);

        ctx.local_ctx.set_active_local(local_index);

        let current_mem_offset = ctx.memory_ctx.offset;
        ctx.global_ctx
            .set_global("mem_offset", ConstExpr::i32_const(current_mem_offset + 1));
        ctx.memory_ctx.alloc(1);

        result.push(Instruction::I32Const(current_mem_offset));
        result.extend(let_value);

        result.push(Instruction::I32Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));

        result.push(Instruction::I32Const(current_mem_offset));

        ctx.code_ctx.add_local(let_type.try_into()?);

        if !ctx.local_ctx.get_already_set() {
            result.push(Instruction::LocalSet(local_index));
        }

        Ok(result)
    }
}

impl<'a> Instructions<'a> for ConstStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let c_expr = match self.value.clone() {
            Expression::Integer(i) => ConstExpr::i32_const(i.value),
            Expression::Boolean(boolean) => ConstExpr::i32_const(if boolean.value { 1 } else { 0 }),
            Expression::String(str) => ConstExpr::i32_const(str.allocate(ctx)),

            // TODO
            _ => panic!(
                "{:?}",
                CompilerError::not_supported_expr("expr", "const", 999)
            ),
        };

        ctx.global_ctx
            .add_global_int(&self.name.value, c_expr, false);

        Ok(vec![])
    }
}

impl<'a> Instructions<'a> for StructStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        // TODO: Too much .clone()
        let mut field_index = 0;
        ctx.type_ctx.new_struct(
            self.name.value.clone(),
            Structure {
                name: self.name.value.clone(),
                fields: self
                    .fields
                    .fields
                    .clone()
                    .into_iter()
                    .map(|(name, ty)| {
                        let res = (name.value, (field_index, ty));
                        field_index += 1;
                        res
                    })
                    .collect(),
            },
        );

        Ok(vec![])
    }
}

impl ObjectExpr {
    pub fn allocate(&self, ctx: &mut Context, size: i32) -> i32 {
        let current_mem_offset = ctx.memory_ctx.offset;
        ctx.global_ctx.set_global(
            "mem_offset",
            ConstExpr::i32_const(current_mem_offset + size as i32),
        );

        ctx.memory_ctx.alloc(size);

        current_mem_offset
    }
}

impl<'a> Instructions<'a> for ObjectExpr {
    // check if struct exists, and allocate memory for the struct init
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        // Try to get struct
        let Some(structure) = ctx.type_ctx.get_struct(&self.name.value).cloned() else {
            return Err(CompilerError::not_defined(
                "struct",
                self.name.value.as_str(),
                0,
            ));
        };

        ctx.type_ctx.set_active_type(Type::Custom(structure.name));

        let ptr = self.allocate(ctx, structure.fields.clone().len() as i32);

        // Check if fields exists
        // and execute the value and put it in memory
        let mut i = 0;
        let mut result: Vec<Instruction> = vec![];
        for (field_name, value) in &self.fields.fields {
            if structure.fields.get(&field_name.value) == None {
                return Err(CompilerError::not_defined(
                    "sturct field",
                    field_name.value.as_str(),
                    0,
                ));
            }

            result.push(Instruction::I32Const(ptr + i));
            result.extend(value.generate_instructions(ctx)?);
            result.push(Instruction::I32Store(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));

            i += 1;
        }

        result.push(Instruction::I32Const(ptr));

        Ok(result.clone())
    }
}

impl<'a> Instructions<'a> for ObjectAccess {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        // Get the variable
        let Some(var_type) = ctx.local_ctx.get_local_type(&self.variable) else {
            return Err(CompilerError::NotDefined(format!(
                "Variable with name {} is not defined!",
                &self.variable
            )));
        };

        let Some(local) = ctx.local_ctx.get_local_index(&self.variable) else {
            return Err(CompilerError::NotDefined(format!(
                "Variable with name {} is not defined!",
                self.variable
            )));
        };

        let mut result = vec![];

        match var_type {
            Type::Custom(struct_name) => {
                let Some(structure) = ctx.type_ctx.get_struct(struct_name) else {
                    return Err(CompilerError::NotDefined(format!(
                        "Structure with name {} is not defined!",
                        &self.variable
                    )));
                };

                let fields = self.fields.clone();
                let mut next_field = Some(Box::new(self.fields.clone()));
                let mut ty = ctx.type_ctx.active_type.clone();

                while let Some(field) = next_field.clone() {
                    let Some(structure_field) = structure.fields.get(&fields.field) else {
                        return Err(CompilerError::NotDefined(format!(
                            "{} structure does't have field named {}!",
                            structure.name, field.field
                        )));
                    };

                    ty = structure_field.1.clone();

                    // Now get the property ( by index )
                    result.push(Instruction::LocalGet(local.index));
                    result.push(Instruction::I32Const(structure_field.0));
                    result.push(Instruction::I32Add);
                    result.push(Instruction::I32Load(MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }));

                    next_field = field.inner_field.clone();
                }

                ctx.type_ctx.set_active_type(ty);
            }

            _ => {
                return Err(CompilerError::NotSupported(
                    "You can't access field of non object type!".to_string(),
                ));
            }
        }

        Ok(result)
    }
}

impl ArrayExpr {
    pub fn allocate(&self, ctx: &mut Context, size: i32) -> i32 {
        let current_mem_offset = ctx.memory_ctx.offset;
        ctx.global_ctx.set_global(
            "mem_offset",
            ConstExpr::i32_const(current_mem_offset + size as i32),
        );

        ctx.memory_ctx.alloc(size);

        current_mem_offset
    }
}

impl<'a> Instructions<'a> for ArrayExpr {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let ptr = self.allocate(ctx, self.items.clone().len() as i32);

        let mut i = 0;
        let mut result: Vec<Instruction> = vec![];

        //let mut first_item_type: Option<Type> = None;

        for item in &self.items {
            result.push(Instruction::I32Const(ptr + i));
            result.extend(item.generate_instructions(ctx)?);
            result.push(Instruction::I32Store(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));

            i += 1;
        }

        ctx.type_ctx
            .set_active_type(Type::Array(Box::new(ctx.type_ctx.active_type.clone())));

        result.push(Instruction::I32Const(ptr));

        Ok(result)
    }
}

impl<'a> Instructions<'a> for BlockStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];

        for statement in &self.statements {
            result.extend(statement.generate_instructions(ctx)?);
        }

        Ok(result)
    }
}

impl<'a> Instructions<'a> for FunctionStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result = self.body.generate_instructions(ctx)?;
        result.push(Instruction::End);

        Ok(result)
    }
}

impl<'a> Instructions<'a> for IfExpr {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result = vec![];
        let condition = self.condition.generate_instructions(ctx)?;
        result.extend(condition);

        result.push(Instruction::If(wasm_encoder::BlockType::Empty));

        let block = self.consequence.generate_instructions(ctx)?;

        result.extend(block);

        // If we are in a let statement
        if let Some(id) = ctx.local_ctx.get_active_local() {
            result.push(Instruction::LocalSet(id.to_owned()));

            ctx.local_ctx.already_set(true);
        }

        match &self.alternative {
            Some(alt) => {
                result.push(Instruction::Else);

                let block = alt.generate_instructions(ctx)?;

                result.extend(block);

                if let Some(id) = ctx.local_ctx.get_active_local() {
                    result.push(Instruction::LocalSet(id.to_owned()));
                }
            }

            None => {}
        };

        result.push(Instruction::End);

        Ok(result)
    }
}

impl<'a> Instructions<'a> for Statement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        match self {
            Statement::Function(func) => {
                let types = func.meta.types()?;
                let type_index = ctx.type_ctx.new_function_type(types.0.clone(), types.1);

                let params = types
                    .0
                    .into_iter()
                    .zip(func.meta.params.clone())
                    .enumerate()
                    .map(|(i, (t, param))| FunctionParam {
                        id: i as u32,
                        param_type: t,
                        name: param.0.value,
                    })
                    .collect::<Vec<FunctionParam>>();

                for (name, ty) in &func.meta.params {
                    ctx.local_ctx
                        .new_local(name.value.to_owned(), ty.to_owned(), false);
                }

                ctx.function_ctx.new_function(
                    type_index,
                    func.meta.name.value.clone(),
                    params,
                    func.meta.return_type.clone(),
                );

                let block = func.generate_instructions(ctx)?;
                ctx.code_ctx
                    .new_function_code(block, func.clone().meta.name.value);

                ctx.local_ctx.reset();

                Ok(vec![])
            }

            Statement::Expression(expr) => expr.generate_instructions(ctx),

            Statement::Let(var) => {
                let let_statement = var.generate_instructions(ctx);

                // Exit the let
                ctx.local_ctx.exit_active_local();
                ctx.local_ctx.already_set(false);

                let_statement
            }
            Statement::Return(ret) => ret.generate_instructions(ctx),
            Statement::Export(export) => export.generate_instructions(ctx),
            Statement::Loop(l) => l.generate_instructions(ctx),
            Statement::Set(set) => set.generate_instructions(ctx),
            Statement::Break(br) => br.generate_instructions(ctx),
            Statement::External(external) => external.generate_instructions(ctx),
            Statement::Builtin(builtin) => builtin.generate_instructions(ctx),
            Statement::Const(cnst) => cnst.generate_instructions(ctx),
            Statement::Struct(strct) => strct.generate_instructions(ctx),
            _ => todo!(),
        }
    }
}

impl<'a> Instructions<'a> for Program {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];

        for statement in &self.statements {
            match statement.generate_instructions(ctx) {
                Ok(instructions) => {
                    result.extend(instructions);
                }
                Err(error) => {
                    ctx.errors.push(error);
                }
            };

            ctx.type_ctx.set_active_type(Type::I32);
        }

        Ok(result)
    }
}

/// Codegen context
pub struct Context {
    /// The result of the parser
    ast: Program,

    /// Final source
    module: Module,

    /// Collected Errors when compiling
    pub(crate) errors: Vec<CompilerError>,

    /// Manages types like function types, or structs
    pub(crate) type_ctx: TypeContext,

    /// Manages functions
    pub(crate) function_ctx: FunctionContext,

    /// Manages Codes
    pub(crate) code_ctx: CodeContext,

    /// Local variables context
    pub(crate) local_ctx: LocalContext,

    /// Memory (heap) Context
    pub memory_ctx: MemoryContext,

    /// Starting point of the memory
    pub(crate) memory_offset: i32,

    /// Exported functions or memory context
    pub(crate) export_ctx: ExportContext,

    /// TODO: <write>
    pub(crate) import_ctx: ImportContext,

    /// Global variables context
    pub(crate) global_ctx: GlobalContext,
}

impl Context {
    /// Creates the new Context
    pub fn new(program: Program, memory_offset: i32) -> Self {
        let mem = MemoryType {
            minimum: 5,
            maximum: None,
            memory64: false,
            shared: false,
        };

        Self {
            ast: program,
            errors: vec![],
            type_ctx: TypeContext::new(),
            module: Module::new(),
            code_ctx: CodeContext::new(),
            function_ctx: FunctionContext::new(),
            local_ctx: LocalContext::new(),
            export_ctx: ExportContext::new(),
            import_ctx: ImportContext::new(),
            global_ctx: GlobalContext::new(),
            //builtin_context: BuiltinContext::new(),
            memory_ctx: MemoryContext::new(mem, memory_offset),
            memory_offset,
        }
    }

    pub fn visit(&mut self) -> CResult<()> {
        let ast = self.ast.clone();

        ast.generate_instructions(self)?;

        Ok(())
    }

    pub fn import_module(
        &mut self,
        module: &str,
        function_name: &str,
        function_arg_types: Vec<Type>,
        function_return_types: Vec<Type>,
    ) -> CResult<()> {
        let function_arg_types = types_to_val_types(function_arg_types)?;
        // Create type for function
        let type_id = self.type_ctx.new_function_type(
            function_arg_types.clone(),
            types_to_val_types(function_return_types.clone())?,
        );

        self.import_ctx
            .import_func(module, function_name, EntityType::Function(type_id));

        self.function_ctx.new_external_function(
            function_name.to_string(),
            function_arg_types
                .into_iter()
                .enumerate()
                .map(|(i, ty)| FunctionParam {
                    id: i as u32,
                    name: String::new(),
                    param_type: ty,
                })
                .collect(),
            function_return_types.get(0).map(|v| v.clone()),
        );

        Ok(())
    }

    /// Bootstraps the default variables
    /// like memory offset
    pub fn bootstrap(&mut self) {
        self.global_ctx.add_global_int(
            "mem_offset",
            ConstExpr::i32_const(self.memory_offset),
            true,
        );
    }

    /// Returns the errors
    pub fn get_errors(&self) -> &Vec<CompilerError> {
        &self.errors
    }

    pub fn generate(&mut self) -> Vec<u8> {
        // export memory
        self.export_ctx.export_memory("memory", 0);
        self.export_ctx.export_global(&"mem_offset".to_string(), 0);

        //TODO
        self.module.section(&self.type_ctx.get_section());
        self.module.section(&self.import_ctx.get_section());
        self.module.section(&self.function_ctx.get_section());
        self.module.section(&TableSection::new());

        let (mem_section, data_section) = &self.memory_ctx.get_sections();

        self.module.section(mem_section);

        self.global_ctx.apply_globals();
        self.module.section(&self.global_ctx.get_section());

        self.module.section(&self.export_ctx.get_section());
        self.module.section(&ElementSection::new());

        self.module.section(&self.code_ctx.get_section());
        self.module.section(data_section);

        self.module.clone().finish()
    }
}

/// Fig Structure type
#[derive(Clone)]
pub struct Structure {
    name: String,
    // (Name, (index, type))
    fields: HashMap<String, (i32, Type)>,
}

impl Structure {
    // TODO: this not works correctly
    pub fn length_width(&self) -> usize {
        self.fields.len()
    }
}

// TODO: create use function_type method
pub struct TypeContext {
    section: TypeSection,
    types_index: u32,
    active_type: Type,
    structs: HashMap<String, Structure>,
}

impl TypeContext {
    pub fn new() -> Self {
        Self {
            section: TypeSection::new(),
            types_index: 0,
            active_type: Type::I32,
            structs: HashMap::new(),
        }
    }

    pub fn new_function_type(
        &mut self,
        param_type: Vec<ValType>,
        result_type: Vec<ValType>,
    ) -> u32 {
        self.section.function(param_type, result_type);

        let index = self.types_index;

        self.types_index += 1;

        index
    }

    pub fn get_section(&self) -> TypeSection {
        self.section.clone()
    }

    pub fn set_active_type(&mut self, new_active_ty: Type) {
        self.active_type = new_active_ty;
    }

    pub fn new_struct(&mut self, name: String, structure: Structure) {
        self.structs.insert(name, structure);
    }

    pub fn get_struct(&self, name: &String) -> Option<&Structure> {
        self.structs.get(name)
    }
}

#[derive(Debug, Clone)]
pub struct FunctionData {
    name: String,
    params: Vec<FunctionParam>,
    return_type: Option<Type>,
    id: u32,
}

#[derive(Debug, Clone)]
pub struct FunctionParam {
    id: u32,
    name: String,
    param_type: ValType,
}

pub struct FunctionContext {
    functions: HashMap<String, FunctionData>,
    section: FunctionSection,
    functions_index: u32,
    current_function: Option<FunctionData>,
}

impl FunctionContext {
    pub fn new() -> Self {
        Self {
            section: FunctionSection::new(),
            functions_index: 0,
            functions: HashMap::new(),
            current_function: None,
        }
    }

    pub fn current_function(&self) -> Option<FunctionData> {
        self.current_function.clone()
    }

    pub fn get_function(&self, function_name: &String) -> Option<&FunctionData> {
        self.functions.get(function_name)
    }

    pub fn new_external_function(
        &mut self,
        name: String,
        params: Vec<FunctionParam>,
        return_type: Option<Type>,
    ) {
        let new_fn = FunctionData {
            name: name.clone(),
            params,
            return_type,
            id: self.functions_index,
        };

        self.functions.insert(name, new_fn.clone());
        self.functions_index += 1;

        //self.section.function(index);
    }

    pub fn new_function(
        &mut self,
        type_index: u32,
        name: String,
        params: Vec<FunctionParam>,
        return_type: Option<Type>,
    ) {
        let new_fn = FunctionData {
            name: name.clone(),
            params,
            return_type,
            id: self.functions_index,
        };

        self.functions.insert(name, new_fn.clone());

        self.current_function = Some(new_fn);

        self.functions_index += 1;

        self.section.function(type_index);
    }

    pub fn get_section(&self) -> FunctionSection {
        self.section.clone()
    }
}

pub struct CodeContext {
    section: CodeSection,
    current_locals: Vec<ValType>,
}

impl CodeContext {
    pub fn new() -> Self {
        Self {
            section: CodeSection::new(),
            current_locals: vec![],
        }
    }

    // TODO: FIX
    pub fn new_function_code(&mut self, instructions: Vec<Instruction>, _function_name: String) {
        let mut func = Function::new_with_locals_types(self.current_locals.clone());

        // idk is this ok?
        self.current_locals.clear();

        for instruction in &instructions {
            func.instruction(instruction);
        }

        self.section.function(&func);
    }

    pub fn add_local(&mut self, local: ValType) {
        self.current_locals.push(local)
    }

    pub fn get_section(&self) -> CodeSection {
        self.section.clone()
    }
}

pub struct Local {
    pub(super) derefable: bool,
    pub(super) index: u32,
}

pub struct LocalContext {
    /// name, id
    ///
    /// for example when new let were created
    /// new entry in this hashmap with (let name, index)
    ///
    /// witch first let index is 0 second is 1 and so on
    locals: HashMap<String, Local>,

    locals_type: HashMap<String, Type>,

    /// Index
    locals_index: u32,

    /// The current active local
    ///
    /// Mostly used for return value of the if statement
    /// to set to the active_local
    active_local: Option<u32>,

    /// Some times the let dont need to set
    /// the expression to the local
    ///
    /// and its already seted
    already_set: bool,
}

impl LocalContext {
    /// Create new local ctx
    pub fn new() -> Self {
        Self {
            locals: HashMap::new(),
            locals_type: HashMap::new(),
            locals_index: 0,
            active_local: None,
            already_set: false,
        }
    }

    //pub fn local_exists(&self, name: &String) -> bool {
    //    self.locals.contains_key(name)
    //}

    pub fn get_local_index(&self, name: &String) -> Option<&Local> {
        self.locals.get(name)
    }

    /// When we create a new function
    pub fn reset(&mut self) {
        self.locals_index = 0;
        self.locals.clear();
    }

    pub fn exit_active_local(&mut self) {
        self.active_local = None;
    }

    pub fn get_active_local(&self) -> &Option<u32> {
        &self.active_local
    }

    pub fn set_active_local(&mut self, local_id: u32) {
        self.active_local = Some(local_id);
    }

    pub fn already_set(&mut self, new_value: bool) {
        self.already_set = new_value;
    }

    pub fn get_already_set(&self) -> &bool {
        &self.already_set
    }

    /// Returns the type of local
    pub fn get_local_type(&self, name: &String) -> Option<&Type> {
        self.locals_type.get(name)
    }

    /// Creates new local var
    ///
    /// and returns the index
    /// if its exists will overwrite it
    pub fn new_local(&mut self, name: String, ty: Type, derefable: bool) -> u32 {
        let index = self.locals_index;

        self.locals.insert(
            name.clone(),
            Local {
                index: self.locals_index.clone(),
                derefable,
            },
        );
        self.locals_type.insert(name, ty);

        self.locals_index += 1;

        index
    }
}

pub struct MemoryContext {
    memory_section: MemorySection,
    data_section: DataSection,

    pub offset: i32,
}

impl MemoryContext {
    pub fn new(memory: MemoryType, starting_offset: i32) -> Self {
        let mut memory_section = MemorySection::new();
        memory_section.memory(memory);

        Self {
            memory_section,
            data_section: DataSection::new(),
            offset: starting_offset,
        }
    }

    /// Returns pointer to the data
    pub fn alloc_data<D>(&mut self, size: i32, data: D) -> i32
    where
        D: IntoIterator<Item = u8>,
        D::IntoIter: ExactSizeIterator,
    {
        // Store the before value, we will need this later
        let ptr = self.offset;
        let offset = ConstExpr::i32_const(ptr);

        self.data_section.active(0, &offset, data);

        self.offset += size;

        ptr
    }

    pub fn alloc(&mut self, size: i32) -> i32 {
        let ptr = self.offset;

        self.offset += size;

        ptr
    }

    pub fn get_sections(&self) -> (MemorySection, DataSection) {
        (self.memory_section.clone(), self.data_section.clone())
    }
}

pub struct ExportContext {
    section: ExportSection,
}

impl ExportContext {
    pub fn new() -> Self {
        Self {
            section: ExportSection::new(),
        }
    }

    pub fn export_memory(&mut self, name: &str, id: u32) {
        self.section.export(name, ExportKind::Memory, id);
    }

    pub fn export_function(&mut self, name: &String, id: u32) {
        self.section.export(name, ExportKind::Func, id);
    }

    pub fn export_global(&mut self, name: &String, id: u32) {
        self.section.export(name, ExportKind::Global, id);
    }

    pub fn get_section(&self) -> ExportSection {
        self.section.clone()
    }
}

pub struct ImportContext {
    section: ImportSection,
}

impl ImportContext {
    pub fn new() -> Self {
        Self {
            section: ImportSection::new(),
        }
    }

    pub fn import_func(&mut self, module: &str, function_name: &str, function_type: EntityType) {
        self.section.import(module, function_name, function_type);
    }

    pub fn get_section(&self) -> ImportSection {
        self.section.clone()
    }
}

pub struct GlobalContext {
    section: GlobalSection,

    // We will search in this Vec, it's O(n)
    // but i dont care
    globals: VecDeque<(String, u32, GlobalType, ConstExpr)>,
    globals_id: u32,
}

impl GlobalContext {
    pub fn new() -> Self {
        Self {
            section: GlobalSection::new(),
            globals: VecDeque::new(),
            globals_id: 0,
        }
    }

    /// Adds global integer
    pub fn add_global_int(&mut self, name: &str, init: ConstExpr, mutable: bool) -> u32 {
        self.globals.push_back((
            name.to_string(),
            self.globals_id,
            GlobalType {
                val_type: ValType::I32,
                mutable,
            },
            init,
        ));

        self.globals_id += 1;

        self.globals_id
    }

    /// Start pop all the globals and apply them
    pub fn apply_globals(&mut self) {
        while let Some((_name, _id, ty, expr)) = self.globals.pop_front() {
            self.section.global(ty, &expr);
        }
    }

    // Ugly ass code
    // TODO: return Error if glob is None
    pub fn set_global(&mut self, name: &str, value: ConstExpr) {
        let glob = self
            .globals
            .iter_mut()
            .position(|(n, _, _, _)| n == &name.to_string())
            .unwrap();

        self.globals[glob] = (
            self.globals[glob].0.clone(),
            self.globals[glob].1,
            self.globals[glob].2,
            value,
        );
    }

    pub fn get_global(&self, name: &String) -> Option<(u32, &GlobalType, &ConstExpr)> {
        let Some((_, id, ty, expr)) = self.globals.iter().find(|(n, _, _, _)| n == name) else {
            return None;
        };

        Some((id.clone(), ty, expr))
    }

    pub fn get_section(&self) -> GlobalSection {
        self.section.clone()
    }
}
