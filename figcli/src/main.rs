use clap::{Args, Parser, Subcommand};
use figc::codegen::codegen::Context;
use figc::lexer::lexer::Lexer;
use figc::parser::ast::Program;
use figc::parser::parser::Parser as FigParser;
use figc::preprocessor::preprocessor::Preprocessor;
use figc::types::types::Type;
use log::info;
use serde::{Deserialize, Serialize};
use std::fs::{self, read_to_string, File};
use std::io::prelude::*;
use std::io::{BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::exit;
use wasmer::{
    imports, Function, FunctionEnv, FunctionEnvMut, Instance, Memory, Module, Store, TypedFunction,
    Value, WasmPtr,
};
use wasmer_wasix::{generate_import_object_from_env, WasiEnv};
use wasmprinter::print_bytes;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Compiles Fig file to an Wasm module
    Compile(CompileArgs),

    Run(RunArgs),

    Server(ServerArgs),

    New(NewProjectArgs),
}

#[derive(Args, Debug)]
struct NewProjectArgs {
    name: String,

    #[arg(long, action, default_value = "fig")]
    target: String,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Fig file path
    fig_file_path: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ServerArgs {
    /// Fig file path
    fig_file_path: PathBuf,

    #[arg(long, action)]
    addr: String,
}

#[derive(Args, Debug)]
struct CompileArgs {
    /// Fig file path
    fig_file_path: PathBuf,

    #[arg(long, action)]
    /// Print Wat
    print_wat: bool,
}

/// Expects SourceCode returns Wasm binary
fn fig_compile_to_wasm(
    source: String,
    memory_offset: i32,
    imports: Vec<(&str, &str, Vec<Type>, Vec<Type>)>,
) -> (Vec<u8>, i32) {
    info!("Starting to compile ...");
    info!("Lexing source ...");
    let mut lexer = Lexer::new(source);
    info!("Parsing tokens ...");
    let mut parser = FigParser::new(&mut lexer);
    let program = Program::parse(&mut parser, None);

    info!("{:?}", program);

    for error in program.get_errors() {
        println!("{}", error);
    }

    let mut preprocessor = Preprocessor::new(program);

    preprocessor.add_module(
        "std".to_string(),
        include_str!("../../std/std.fig").to_string(),
    );

    preprocessor.add_module(
        "server".to_string(),
        include_str!("../../std/server.fig").to_string(),
    );

    let program = preprocessor.process();

    for error in program.get_errors() {
        println!("{}", error);
    }

    let mut ctx = Context::new(program, memory_offset);
    for (module_name, function_name, function_arg_types, function_return_types) in imports {
        ctx.import_module(
            module_name,
            function_name,
            function_arg_types,
            function_return_types,
        )
        // TODO: remove unwrap()
        .unwrap();
    }
    ctx.bootstrap();
    ctx.visit().unwrap();

    for error in ctx.get_errors() {
        println!("{}", error);
        exit(1);
    }

    let buf = ctx.generate();

    (buf, ctx.memory_ctx.offset)
}

#[derive(Clone)]
struct Env {
    memory: Option<Memory>,
    alloc_func: Option<TypedFunction<i32, i32>>,
}

fn fig_print(mut env: FunctionEnvMut<Env>, p: WasmPtr<u8>) {
    let (env_data, store) = env.data_and_store_mut();
    let memory_view = env_data.memory.clone().unwrap().view(&store);

    println!("{}", p.read_utf8_string_with_nul(&memory_view).unwrap());
}

fn fig_print_int(int: i32) {
    println!("{}", int);
}

fn fig_print_char(c: i32) {
    println!("{}", char::from(c as u8));
}

fn fig_print_float(f: f32) {
    println!("{}", f);
}

fn fig_exit(code: i32) {
    exit(code);
}

fn fig_read_file(mut env: FunctionEnvMut<Env>, addr: WasmPtr<u8>) -> i32 {
    let (env_data, mut store) = env.data_and_store_mut();
    let mut file_content: Vec<u8> = Vec::new();

    {
        let memory_view = env_data.memory.clone().unwrap().view(&mut store);

        let addr_str = addr.read_utf8_string_with_nul(&memory_view).unwrap();

        let mut file = File::open(addr_str).unwrap();

        file.read_to_end(&mut file_content).unwrap();
    }

    let buf = env_data
        .alloc_func
        .clone()
        .unwrap()
        .call(&mut store, file_content.len() as i32 + 1)
        .unwrap();

    let memory_view = env_data.memory.clone().unwrap().view(&mut store);

    let buf_ptr = WasmPtr::<u8>::new(buf as u32);
    memory_view
        .write(buf_ptr.offset() as u64, &file_content)
        .unwrap();

    buf
}

fn run_wasm(bytes: &Vec<u8>) {
    let mut store = Store::default();
    let module = Module::new(&store, bytes).unwrap();

    let env = FunctionEnv::new(
        &mut store,
        Env {
            memory: None,
            alloc_func: None,
        },
    );

    let import_object = imports! {
        "io" => {
            "print_str" => Function::new_typed_with_env(&mut store, &env, fig_print),
            "print_int" => Function::new_typed(&mut store, fig_print_int),
            "print_char" => Function::new_typed(&mut store, fig_print_char),
            "print_float" => Function::new_typed(&mut store,  fig_print_float),
            "read_file" => Function::new_typed_with_env(&mut store, &env, fig_read_file),
        },
        "sys" => {
            "exit" => Function::new_typed(&mut store, fig_exit),
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();

    let wasi = WasiEnv::builder("wasm-app")
        .imports(import_object.into_iter())
        .build()
        .unwrap();

    let wasi_env = FunctionEnv::new(&mut store, wasi);
    let wasi_imports = generate_import_object_from_env(
        &mut store,
        &wasi_env,
        wasmer_wasix::WasiVersion::Snapshot0,
    );

    let instance = Instance::new(&mut store, &module, &import_object).unwrap();
    let memory = instance.exports.get_memory("memory").unwrap().clone();

    env.as_mut(&mut store).memory = Some(memory.clone());
    let main_fn = instance.exports.get_function("main").unwrap();
    let result = main_fn.call(&mut store, &[]);
    match result {
        Ok(_) => {}
        Err(e) => {
            // Log the error
            println!("Error caught from `main`: {}", e.message());

            // Errors come with a trace we can inspect to get more
            // information on the execution flow.
            let frames = e.trace();
            let frames_len = frames.len();

            for i in 0..frames_len {
                println!(
                    "  Frame #{}: {:?}::{:?}",
                    frames_len - i,
                    frames[i].module_name(),
                    frames[i].function_name().or(Some("<func>")).unwrap()
                );
            }
        }
    }

    info!(
        "Final Memory: {:?}",
        memory
            .view(&store)
            .copy_to_vec()
            .unwrap()
            .into_iter()
            .filter(|v| *v != 0)
            .collect::<Vec<u8>>()
    );
}

fn run_wasm_server<'a>(bytes: &Vec<u8>, addr: &'a str, mem_offset: i32) {
    let mut store = Store::default();
    let module = Module::new(&store, bytes).unwrap();

    let env = FunctionEnv::new(
        &mut store,
        Env {
            memory: None,
            alloc_func: None,
        },
    );
    let import_object = imports! {
        "io" => {
            "print_str" => Function::new_typed_with_env(&mut store, &env, fig_print),
            "print_int" => Function::new_typed(&mut store, fig_print_int),
            "print_char" => Function::new_typed(&mut store, fig_print_char),
            "print_float" => Function::new_typed(&mut store,  fig_print_float),
            "read_file" => Function::new_typed_with_env(&mut store, &env, fig_read_file),
        },
        "sys" => {
            "exit" => Function::new_typed(&mut store, fig_exit),
        }
    };

    let listener = TcpListener::bind(addr).unwrap();

    for stream in listener.incoming() {
        let instance = Instance::new(&mut store, &module, &import_object).unwrap();
        let memory = instance.exports.get_memory("memory").unwrap().clone();
        let mem_offset_glob = instance.exports.get_global("mem_offset").unwrap();
        let alloc_func = instance
            .exports
            .get_typed_function(&store, "malloc")
            .unwrap();
        env.as_mut(&mut store).memory = Some(memory);
        env.as_mut(&mut store).alloc_func = Some(alloc_func.clone());
        let main_fn = instance.exports.get_function("main").unwrap();

        let mut stream = stream.unwrap();

        let buf_reader = BufReader::new(&mut stream);
        let http_request: String = buf_reader
            .lines()
            .map(|result| result.unwrap())
            .take_while(|line| !line.is_empty())
            .collect();

        mem_offset_glob
            .set(
                &mut store,
                Value::I32(mem_offset + http_request.len() as i32 + 1),
            )
            .unwrap();

        let mem = env.as_mut(&mut store).memory.clone().unwrap();
        let view = mem.view(&store);
        view.write(mem_offset as u64, &http_request.as_bytes())
            .unwrap();

        let response_value = main_fn.call(&mut store, &[Value::I32(mem_offset)]).unwrap();

        let response_ptr = WasmPtr::<u8>::new(response_value[0].i32().unwrap() as u32);

        let mem = env.as_mut(&mut store).memory.clone().unwrap();
        let view = mem.view(&store);
        let response = response_ptr.read_utf8_string_with_nul(&view).unwrap();

        stream.write(response.as_bytes()).unwrap();
        stream.flush().unwrap();
    }
}

/// Fig config file
#[derive(Deserialize, Serialize)]
struct Config {
    package: PackageInfo,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum ProgramTarget {
    Web,
    Wasmer,
    Fig,
}

impl From<String> for ProgramTarget {
    fn from(value: String) -> Self {
        match value.as_str() {
            "web" => Self::Web,
            "wasmer" => Self::Wasmer,
            "fig" => Self::Fig,
            _ => Self::Fig,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct PackageInfo {
    name: String,
    target: ProgramTarget,
}

// TODO: impl Error handling
fn main() {
    env_logger::init();

    let cli = Cli::parse();

    match &cli.command {
        Commands::New(new_args) => {
            let src_path = format!("{}/src", new_args.name.clone());
            // Create a dir named new_args.name
            // and /src folder
            fs::create_dir_all(&src_path).unwrap();

            let main_source = "export fn main() {\n    print_str(\"HelloWorld\");\n}";

            let mut main_file = File::create_new(format!("{}/main.fig", src_path)).unwrap();
            main_file.write_all(main_source.as_bytes()).unwrap();

            let config = Config {
                package: PackageInfo {
                    name: new_args.name.clone(),
                    target: ProgramTarget::from(new_args.target.clone()),
                },
            };

            let toml_text = toml::to_string(&config).unwrap();

            let mut main_file = File::create_new(format!("{}/fig.toml", new_args.name)).unwrap();
            main_file.write_all(toml_text.as_bytes()).unwrap();

            println!("New project created!");
        }
        Commands::Compile(c_args) => {
            // Open file
            let mut buffer = String::new();
            {
                let mut file = File::open(&c_args.fig_file_path).unwrap();
                file.read_to_string(&mut buffer).unwrap();
                file.flush().unwrap();
            }

            // Now compile
            let (final_wasm, _offset) = fig_compile_to_wasm(buffer, 0x0, vec![]);

            if c_args.print_wat {
                let wasm = print_bytes(&final_wasm)
                    .unwrap_or("Could't print compiled wasm to wat!".to_string());

                println!("{}", wasm);
            };

            // Create A Compiled wasm file
            let wasm_file_path = PathBuf::from(format!(
                "./{}.wasm",
                c_args.fig_file_path.file_stem().unwrap().to_str().unwrap()
            ));

            let mut wasm_file = File::create(wasm_file_path).unwrap();
            wasm_file.write_all(&*final_wasm).unwrap();
            wasm_file.flush().unwrap();
        }
        Commands::Run(RunArgs {
            fig_file_path: Some(path),
            ..
        }) => {
            let mut buffer = String::new();
            {
                let mut file = File::open(&path).unwrap();
                file.read_to_string(&mut buffer).unwrap();
                file.flush().unwrap();
            }

            // Now compile
            let (final_wasm, _offset) = fig_compile_to_wasm(buffer, 0x0, vec![]);

            run_wasm(&final_wasm);
        }
        Commands::Run(RunArgs {
            fig_file_path: None,
            ..
        }) => {
            // Read fig.toml config file
            let config_file_raw = read_to_string("./fig.toml").unwrap();
            let config: Config = toml::from_str(&config_file_raw).unwrap();
            let imports = if matches!(config.package.target, ProgramTarget::Fig) {
                vec![
                    (
                        "io",
                        "print_str",
                        vec![Type::Array(Box::new(Type::Char))],
                        vec![],
                    ),
                    ("io", "print_int", vec![Type::I32], vec![]),
                ]
            } else {
                vec![]
            };
            let main_content = read_to_string("./src/main.fig").unwrap();

            // Now compile
            let (final_wasm, _offset) = fig_compile_to_wasm(main_content, 0x0, imports);

            run_wasm(&final_wasm);
        }

        Commands::Server(server_args) => {
            let mut buffer = String::new();
            {
                let mut file = File::open(&server_args.fig_file_path).unwrap();
                file.read_to_string(&mut buffer).unwrap();
                file.flush().unwrap();
            }

            // Now compile
            let (final_wasm, offset) = fig_compile_to_wasm(buffer, 0x0, vec![]);

            run_wasm_server(&final_wasm, &server_args.addr, offset);
        }
    }
}
