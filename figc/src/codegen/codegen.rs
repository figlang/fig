use std::collections::{BTreeMap, HashMap};

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataSection, ElementSection, EntityType, ExportKind,
    ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection,
    Instruction, MemArg, MemorySection, MemoryType, Module, TableSection, TypeSection, ValType,
};

use crate::{
    lexer::token::Token,
    parser::ast::{
        BlockStatement, BooleanExpr, BreakStatement, BuiltinStatement, CallExpr, ExportStatement,
        Expression, ExternalStatement, FunctionMeta, FunctionStatement, Identifier, IfExpr,
        IndexExpr, InfixExpr, Integer, LetStatement, LoopStatement, Program, RefValue,
        ReturnStatement, SetStatement, Statement, StringExpr,
    },
    types::types::Type,
};

use super::builtins::{free, malloc};

#[derive(Debug)]
pub enum CompilerError {
    NotDefined(String),
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

        let func_id = match ctx.function_ctx.get_function(&self.function.value) {
            Some(func) => Ok(func.id),
            None => Err(CompilerError::NotDefined(format!(
                "Function with name {} is not defined!",
                self.function.value
            ))),
        }?;

        result.push(Instruction::Call(func_id));

        Ok(result)
    }
}

impl<'a> Instructions<'a> for StringExpr {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
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
            .alloc(size as i32, string.as_bytes().to_vec());

        Ok(vec![Instruction::I32Const(ptr)])
    }
}

impl<'a> Instructions<'a> for RefValue {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        match self.value.as_ref() {
            Expression::Identifier(ident) => {
                let Some(id) = ctx.local_ctx.get_local_index(&ident.value) else {
                    return Err(
                        CompilerError::NotDefined(format!("Variable with name {} is not defined!", &ident.value))
                    );
                };

                return Ok(vec![Instruction::I32Const(*id as i32)]);
            }

            Expression::Index(index) => Ok(index.get_offset(ctx)?),

            _ => panic!(),
        }
    }
}

impl<'a> Instructions<'a> for Token {
    fn generate_instructions(&self, _ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        // Todo check type
        match self {
            Token::Plus => Ok(vec![Instruction::I32Add]),
            Token::Minus => Ok(vec![Instruction::I32Sub]),
            Token::ForwardSlash => Ok(vec![Instruction::I32DivS]),
            Token::Asterisk => Ok(vec![Instruction::I32Mul]),
            Token::Equal => Ok(vec![Instruction::I32Eq]),
            Token::NotEqual => Ok(vec![Instruction::I32Ne]),
            Token::LessThan => Ok(vec![Instruction::I32LeS]),
            Token::GreaterThan => Ok(vec![Instruction::I32GtS]),
            Token::Mod => Ok(vec![Instruction::I32RemS]),

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

impl<'a> Instructions<'a> for InfixExpr {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];
        let left_side = self.left.generate_instructions(ctx)?;
        let operation = self.operator.generate_instructions(ctx)?;
        let right_side = self.right.generate_instructions(ctx)?;

        result.extend(left_side);
        result.extend(right_side);
        result.extend(operation);

        Ok(result)
    }
}

impl<'a> Instructions<'a> for Integer {
    fn generate_instructions(&self, _ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        // TODO: push the correct type
        Ok(vec![Instruction::I32Const(self.value)])
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

            ctx.function_ctx
                .new_external_function(func.name.value.clone(), params);

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
            "malloc" => {
                let type_index = ctx
                    .type_ctx
                    .new_function_type(vec![ValType::I32], vec![ValType::I32]);

                ctx.function_ctx
                    .new_function(type_index, "malloc".to_string(), vec![]);

                ctx.code_ctx.add_local(ValType::I32);
                ctx.code_ctx.new_function_code(malloc());
            }

            "free" => {
                let type_index = ctx.type_ctx.new_function_type(vec![ValType::I32], vec![]);

                ctx.function_ctx
                    .new_function(type_index, "free".to_string(), vec![]);

                ctx.code_ctx.new_function_code(free());
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

        match variable {
            Type::String => {
                offset.extend([
                    Instruction::I32Load(MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }),
                    Instruction::I32Const(65536),
                    Instruction::I32RemS,
                ]);
            }

            _ => {
                offset.push(Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                }));
            }
        }

        Ok(offset)
    }

    fn get_offset<'a>(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];
        // First we want to get the offset,
        // we add the current offset with the self.index
        let Some(variable) = ctx.local_ctx.get_local_index(&self.variable.value) else {
            return Err(CompilerError::NotDefined(
                format!("Variable with name {} is not defined!", self.variable.value)
            ));
        };

        // Is this good solution ?
        result.push(Instruction::LocalGet(variable.clone()));
        result.extend(self.index.generate_instructions(ctx)?);
        result.push(Instruction::I32Add);

        Ok(result)
    }
}

impl<'a> Instructions<'a> for IndexExpr {
    fn generate_instructions(&self, _ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        Ok(vec![Instruction::I32Store8(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        })])
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
                let Some(var_id) = ctx.local_ctx.get_local_index(&ident.value) else {
                    return Err(
                        CompilerError::NotDefined(
                            format!("Variable with name {} is not defined!", ident.value)
                        )
                    );
                };

                result.extend(expression);

                result.push(Instruction::LocalSet(var_id.to_owned()));
            }

            _ => unreachable!(),
        }

        Ok(result)
    }
}

impl<'a> Instructions<'a> for ExportStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let function_instructions = self.value.generate_instructions(ctx)?;
        let Some(current_function) = ctx.function_ctx.current_function() else {
            return Err(CompilerError::NotDefined("Function not defined!".to_string()));
        };

        ctx.export_ctx
            .export_function(&current_function.name, current_function.id);

        Ok(function_instructions)
    }
}

impl<'a> Instructions<'a> for ReturnStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result = vec![];

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
            Expression::Identifier(ident) => Ok(ident.generate_instructions(ctx)?),
            Expression::Call(call) => Ok(call.generate_instructions(ctx)?),
            Expression::String(s) => Ok(s.generate_instructions(ctx)?),
            Expression::If(if_expr) => Ok(if_expr.generate_instructions(ctx)?),
            Expression::Boolean(bool_expr) => Ok(bool_expr.generate_instructions(ctx)?),

            Expression::Index(index_expr) => Ok(index_expr.get_instruction(ctx)?),
            Expression::Ref(ref_expr) => Ok(ref_expr.generate_instructions(ctx)?),

            x => panic!("{:?}", x),
        }
    }
}

impl<'a> Instructions<'a> for Identifier {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let Some(id) = ctx.local_ctx.get_local_index(&self.value) else {
            return Err(
                CompilerError::NotDefined(format!("Variable with name {} is not defined!", self.value))
            );
        };

        Ok(vec![Instruction::LocalGet(id.clone())])
    }
}

impl<'a> Instructions<'a> for LetStatement {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        // create the local and set the active local
        let local_index = ctx
            .local_ctx
            .new_local(self.name.value.clone(), self.value_type.clone());
        ctx.local_ctx.set_active_local(local_index);

        let mut result: Vec<Instruction> = vec![];
        let let_value = self.value.generate_instructions(ctx)?;

        result.extend(let_value);

        // create new local
        ctx.code_ctx.add_local(self.value_type.clone().try_into()?);

        if !ctx.local_ctx.get_already_set() {
            result.push(Instruction::LocalSet(local_index));
        }

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
                        .new_local(name.value.to_owned(), ty.to_owned());
                }

                ctx.function_ctx
                    .new_function(type_index, func.meta.name.value.clone(), params);

                let block = func.generate_instructions(ctx)?;
                ctx.code_ctx.new_function_code(block);

                ctx.local_ctx.reset();

                Ok(vec![])
            }

            Statement::Expression(expr) => expr.generate_instructions(ctx),

            Statement::Let(var) => {
                let let_statement = var.generate_instructions(ctx);

                // Exit the let
                ctx.local_ctx.exit_active_local();
                ctx.local_ctx.already_set(false);

                return let_statement;
            }
            Statement::Return(ret) => ret.generate_instructions(ctx),
            Statement::Export(export) => export.generate_instructions(ctx),
            Statement::Loop(l) => l.generate_instructions(ctx),
            Statement::Set(set) => set.generate_instructions(ctx),
            Statement::Break(br) => br.generate_instructions(ctx),
            Statement::External(external) => external.generate_instructions(ctx),
            Statement::Builtin(builtin) => builtin.generate_instructions(ctx),

            _ => todo!(),
        }
    }
}

impl<'a> Instructions<'a> for Program {
    fn generate_instructions(&self, ctx: &'a mut Context) -> CResult<Vec<Instruction>> {
        let mut result: Vec<Instruction> = vec![];
        for statement in &self.statements {
            result.extend(statement.generate_instructions(ctx)?);
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

    /// Manages types like function types
    pub(crate) type_ctx: TypeContext,

    /// Manages functions
    pub(crate) function_ctx: FunctionContext,

    /// Manages Codes
    pub(crate) code_ctx: CodeContext,

    /// Local variables context
    pub(crate) local_ctx: LocalContext,

    /// Memory (heap) Context
    pub(crate) memory_ctx: MemoryContext,

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

    /// Bootstraps the default variables
    /// like memory offset
    pub fn bootstrap(&mut self) {
        self.global_ctx
            // the value 0 is deferent in some runtimes
            .add_global_int("mem_offset", ConstExpr::i32_const(self.memory_offset), true);
    }

    pub fn generate(&mut self) -> Vec<u8> {
        // export memory
        self.export_ctx.export_memory("memory", 0);

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

pub struct TypeContext {
    section: TypeSection,
    types_index: u32,
}

impl TypeContext {
    pub fn new() -> Self {
        Self {
            section: TypeSection::new(),
            types_index: 0,
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
}

#[derive(Debug, Clone)]
pub struct FunctionData {
    name: String,
    params: Vec<FunctionParam>,
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

    pub fn new_external_function(&mut self, name: String, params: Vec<FunctionParam>) {
        let new_fn = FunctionData {
            name: name.clone(),
            params,
            id: self.functions_index,
        };

        self.functions.insert(name, new_fn.clone());
        self.functions_index += 1;
    }

    pub fn new_function(&mut self, type_index: u32, name: String, params: Vec<FunctionParam>) {
        let new_fn = FunctionData {
            name: name.clone(),
            params,
            id: self.functions_index,
        };

        self.functions.insert(name, new_fn.clone());

        self.section.function(type_index);

        self.current_function = Some(new_fn);

        self.functions_index += 1;
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

    pub fn new_function_code(&mut self, instructions: Vec<Instruction>) {
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
pub struct LocalContext {
    /// name, id
    ///
    /// for example when new let were created
    /// new entry in this hasmap with (let name, index)
    ///
    /// wich first let index is 0 second is 1 and so on
    locals: HashMap<String, u32>,

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

    pub fn get_local_index(&self, name: &String) -> Option<&u32> {
        self.locals.get(name)
    }

    /// When we create a new function
    pub fn reset(&mut self) {
        self.locals_index = 0;
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
    pub fn new_local(&mut self, name: String, ty: Type) -> u32 {
        let index = self.locals_index;

        self.locals.insert(name.clone(), self.locals_index.clone());
        self.locals_type.insert(name, ty);

        self.locals_index += 1;

        index
    }
}

pub struct MemoryContext {
    memory_section: MemorySection,
    data_section: DataSection,

    offset: i32,
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
    pub fn alloc<D>(&mut self, size: i32, data: D) -> i32
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

    pub fn import_func(
        &mut self,
        module: &String,
        function_name: &String,
        function_type: EntityType,
    ) {
        self.section.import(module, function_name, function_type);
    }

    pub fn get_section(&self) -> ImportSection {
        self.section.clone()
    }
}

pub struct GlobalContext {
    section: GlobalSection,
    /// <global_name, id>
    globals: BTreeMap<String, (u32, GlobalType, ConstExpr)>,
    globals_id: u32,
}

impl GlobalContext {
    pub fn new() -> Self {
        Self {
            section: GlobalSection::new(),
            globals: BTreeMap::new(),
            globals_id: 0,
        }
    }

    /// Adds global integer
    pub fn add_global_int(&mut self, name: &str, init: ConstExpr, mutable: bool) -> u32 {
        let id = self.globals_id;
        self.globals.insert(
            name.to_string(),
            (
                id,
                GlobalType {
                    val_type: ValType::I32,
                    mutable,
                },
                init,
            ),
        );

        self.globals_id += 1;

        id
    }

    /// Start pop all the globals and apply them
    pub fn apply_globals(&mut self) {
        while let Some((_key, val)) = self.globals.pop_first() {
            self.section.global(val.1, &val.2);
        }
    }

    pub fn set_global(&mut self, name: &str, value: ConstExpr) {
        let global = self.globals.get_mut(name).unwrap();
        *global = (global.0, global.1, value);
    }

    pub fn get_global_id(&self, name: &String) -> u32 {
        self.globals.get(name).unwrap().0
    }

    pub fn get_section(&self) -> GlobalSection {
        self.section.clone()
    }
}