// SPDX-License-Identifier: MIT

//! The Cranelift backend: *emitting* — each flat-level function translated to Cranelift IR and
//! compiled by Cranelift's JIT.
//!
//! One IR value is held in one or more Cranelift values, its *words* ([`words`]): a string is its
//! 16-byte descriptor as two `i64`s, a range its start, stop, step, and inclusion. A local is one
//! frontend [`Variable`] per word, from which Cranelift's frontend builds SSA form. A runtime check
//! ends its block with a branch to a cold block raising the error, and emission continues in a
//! fresh block: a Cranelift block may end anywhere, so nothing in the IR is reordered.

use cranelift_codegen::Context;
use cranelift_codegen::entity::packed_option::ReservedValue;
use cranelift_codegen::ir::types::{F64, I8, I64};
use cranelift_codegen::ir::{
    AbiParam, Block, FuncRef, GlobalValue, InstBuilder, MemFlagsData, Signature, TrapCode, Type,
    Value,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, default_libcall_names};
use rustc_hash::FxHashMap;

use crate::desugar::desugar;
use crate::flatten::flatten_program;
use crate::ir::{
    BlockId, Flat, Function, FunctionId, GlobalId, IdVec, LocalId, NodeId, Operation, Program,
    ProgramTables, Terminator,
};
use crate::parser::{PinpType, ProgramAst};

mod array;
mod string;
mod value;

#[cfg(test)]
mod tests;

unsafe extern "C" {
    /// libm's `pow`, what `^` computes.
    fn pow(base: f64, exponent: f64) -> f64;
}

/// Name of the entry function: the top-level statements, writing the program's result through
/// its one argument.
const ENTRY: &str = "__pinp_main";

/// The Cranelift types of the words holding a value of `pinp_type`.
fn words(pinp_type: PinpType) -> &'static [Type] {
    match pinp_type {
        PinpType::Int => &[I64],
        PinpType::Float => &[F64],
        // Cranelift has no 1-bit type; a bool is a byte holding 0 or 1.
        PinpType::Bool => &[I8],
        PinpType::Str => &[I64, I64],
        PinpType::Range => &[I64, I64, I64, I8],
        PinpType::Array(..) | PinpType::Matrix(..) => &[I64],
        PinpType::Void => &[],
    }
}

/// The Cranelift values holding one IR value: none for `void`, up to four for a range.
#[derive(Debug, Clone, Copy)]
struct Words {
    values: [Value; 4],
    count: usize,
}

impl Words {
    fn none() -> Self {
        Words {
            values: [Value::reserved_value(); 4],
            count: 0,
        }
    }

    fn new(values: &[Value]) -> Self {
        let mut words = Words::none();
        words.values[..values.len()].copy_from_slice(values);
        words.count = values.len();
        words
    }

    fn one(value: Value) -> Self {
        Words::new(&[value])
    }

    fn as_slice(&self) -> &[Value] {
        &self.values[..self.count]
    }

    /// The only word of a one-word value.
    fn single(&self) -> Value {
        debug_assert_eq!(self.count, 1, "a one-word value");
        self.values[0]
    }
}

/// Wraps a Cranelift error as a message string.
fn error(error: impl std::fmt::Display) -> String {
    format!("Cranelift error: {error}.")
}

/// A program compiled by Cranelift, owning the memory its code lives in.
pub(super) struct Compiled {
    module: Option<Box<JITModule>>,
    entry: unsafe extern "C" fn(*mut u8),
}

impl Compiled {
    /// The program's entry, `void (*)(void *result)`.
    pub(super) fn entry(&self) -> unsafe extern "C" fn(*mut u8) {
        self.entry
    }
}

impl Drop for Compiled {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            // SAFETY: nothing calls into the code once the program is dropped.
            unsafe { module.free_memory() };
        }
    }
}

/// Desugars, flattens, and emits a type-checked program, then JIT-compiles it; also returns the
/// type of the program's result.
pub(super) fn compile(ast: &ProgramAst) -> Result<(Compiled, PinpType), String> {
    let Program {
        tables,
        functions,
        entry,
    } = flatten_program(desugar(ast));
    let result_type = tables.types.get(tables.signatures[entry].result);

    let verifier = if cfg!(debug_assertions) {
        "true"
    } else {
        "false"
    };
    let flags = [("opt_level", "speed"), ("enable_verifier", verifier)];
    let mut builder = JITBuilder::with_flags(&flags, default_libcall_names()).map_err(error)?;
    // libm is linked only when something references it, so `dlsym` alone may not find `pow`.
    builder.symbol("pow", pow as *const u8);
    let mut emitter = ProgramEmitter::new(JITModule::new(builder), &tables, ast, entry)?;
    let mut context = emitter.module.make_context();
    let mut builder_context = FunctionBuilderContext::new();
    for (_, function) in functions.iter() {
        emitter.define(function, &mut context, &mut builder_context)?;
    }

    let entry_id = emitter.functions[entry];
    let mut module = emitter.module;
    module.finalize_definitions().map_err(error)?;
    let code = module.get_finalized_function(entry_id);
    // SAFETY: the entry was declared with the out-pointer signature, in the platform's convention.
    let entry = unsafe { std::mem::transmute::<*const u8, unsafe extern "C" fn(*mut u8)>(code) };
    let compiled = Compiled {
        module: Some(Box::new(module)),
        entry,
    };
    Ok((compiled, result_type))
}

/// What emitting every function shares: the module, and what is declared in it.
struct ProgramEmitter<'program> {
    module: JITModule,
    tables: &'program ProgramTables,
    string_literals: &'program [String],
    entry: FunctionId,
    functions: IdVec<FunctionId, FuncId>,
    globals: IdVec<GlobalId, DataId>,
    runtime_functions: FxHashMap<&'static str, FuncId>,
    /// NUL-terminated byte strings — string literals and error messages — by content.
    texts: FxHashMap<Vec<u8>, DataId>,
}

impl<'program> ProgramEmitter<'program> {
    /// Declares every function and global of the program.
    fn new(
        module: JITModule,
        tables: &'program ProgramTables,
        ast: &'program ProgramAst,
        entry: FunctionId,
    ) -> Result<Self, String> {
        let mut emitter = ProgramEmitter {
            module,
            tables,
            string_literals: &ast.string_literals,
            entry,
            functions: IdVec::default(),
            globals: IdVec::default(),
            runtime_functions: FxHashMap::default(),
            texts: FxHashMap::default(),
        };
        for (function_id, _) in tables.signatures.iter() {
            let signature = emitter.signature(function_id);
            let (name, linkage) = match function_id == entry {
                true => (ENTRY.to_string(), Linkage::Export),
                false => (format!("f{}", function_id.index()), Linkage::Local),
            };
            let func_id = emitter
                .module
                .declare_function(&name, linkage, &signature)
                .map_err(error)?;
            emitter.functions.push(func_id);
        }
        for (global_id, global) in tables.globals.iter() {
            let name = format!("g{}", global_id.index());
            let data_id = emitter
                .module
                .declare_data(&name, Linkage::Local, true, false)
                .map_err(error)?;
            let size = words(tables.types.get(global.type_id)).len().max(1) * 8;
            let mut description = DataDescription::new();
            description.define_zeroinit(size);
            description.set_align(8);
            emitter
                .module
                .define_data(data_id, &description)
                .map_err(error)?;
            emitter.globals.push(data_id);
        }
        Ok(emitter)
    }

    /// A pinp function's signature: each parameter and the result split into words. The entry
    /// takes the address its result is written to instead.
    fn signature(&self, function_id: FunctionId) -> Signature {
        let mut signature = self.module.make_signature();
        if function_id == self.entry {
            signature.params.push(AbiParam::new(I64));
            return signature;
        }
        let pinp_signature = &self.tables.signatures[function_id];
        for &parameter in &pinp_signature.parameters {
            let parameter_type = self.tables.types.get(parameter);
            signature.params.extend(
                words(parameter_type)
                    .iter()
                    .map(|&word| AbiParam::new(word)),
            );
        }
        let result_type = self.tables.types.get(pinp_signature.result);
        signature
            .returns
            .extend(words(result_type).iter().map(|&word| AbiParam::new(word)));
        signature
    }

    fn define(
        &mut self,
        function: &Function<Flat>,
        context: &mut Context,
        builder_context: &mut FunctionBuilderContext,
    ) -> Result<(), String> {
        context.func.signature = self.signature(function.id);
        let target = self.module.target_config();
        let builder = FunctionBuilder::new(&mut context.func, builder_context);
        let mut emitter = FunctionEmitter::new(self, builder, function);
        emitter.emit_body();
        emitter.builder.finalize(target);
        let func_id = self.functions[function.id];
        self.module
            .define_function(func_id, context)
            .map_err(error)?;
        self.module.clear_context(context);
        Ok(())
    }

    /// A function of the runtime library or libc, declared on first use.
    fn runtime_function(
        &mut self,
        name: &'static str,
        params: &[Type],
        returns: &[Type],
    ) -> FuncId {
        if let Some(&func_id) = self.runtime_functions.get(name) {
            return func_id;
        }
        let mut signature = self.module.make_signature();
        signature
            .params
            .extend(params.iter().map(|&param| AbiParam::new(param)));
        signature
            .returns
            .extend(returns.iter().map(|&ret| AbiParam::new(ret)));
        let func_id = self
            .module
            .declare_function(name, Linkage::Import, &signature)
            .expect("a runtime function is declared once, with one signature");
        self.runtime_functions.insert(name, func_id);
        func_id
    }

    /// Read-only data holding `content` and a terminating NUL, defined on first use.
    fn text(&mut self, content: &[u8]) -> DataId {
        if let Some(&data_id) = self.texts.get(content) {
            return data_id;
        }
        let data_id = self
            .module
            .declare_anonymous_data(false, false)
            .expect("anonymous data is always declarable");
        let mut bytes = content.to_vec();
        bytes.push(0);
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        self.module
            .define_data(data_id, &description)
            .expect("new data is defined once");
        self.texts.insert(content.to_vec(), data_id);
        data_id
    }
}

/// Emits one function.
struct FunctionEmitter<'emit, 'program> {
    program: &'emit mut ProgramEmitter<'program>,
    builder: FunctionBuilder<'emit>,
    function: &'emit Function<Flat>,
    blocks: IdVec<BlockId, Block>,
    /// Each local's words, from `first_variable[local]` on.
    variables: Vec<Variable>,
    first_variable: IdVec<LocalId, usize>,
    function_refs: FxHashMap<FuncId, FuncRef>,
    data_values: FxHashMap<DataId, GlobalValue>,
    /// The entry's result address.
    result_address: Option<Value>,
}

impl<'emit, 'program> FunctionEmitter<'emit, 'program> {
    fn new(
        program: &'emit mut ProgramEmitter<'program>,
        mut builder: FunctionBuilder<'emit>,
        function: &'emit Function<Flat>,
    ) -> Self {
        let mut blocks = IdVec::default();
        for _ in function.shape.blocks.iter() {
            blocks.push(builder.create_block());
        }
        let mut variables = Vec::new();
        let mut first_variable = IdVec::default();
        for (_, local) in function.arena.locals.iter() {
            first_variable.push(variables.len());
            for &word in words(program.tables.types.get(local.type_id)) {
                variables.push(builder.declare_var(word));
            }
        }
        FunctionEmitter {
            program,
            builder,
            function,
            blocks,
            variables,
            first_variable,
            function_refs: FxHashMap::default(),
            data_values: FxHashMap::default(),
            result_address: None,
        }
    }

    fn emit_body(&mut self) {
        let entry = self.function.shape.entry;
        self.builder
            .append_block_params_for_function_params(self.blocks[entry]);
        let function = self.function;
        for (block_id, block) in function.shape.blocks.iter() {
            self.builder.switch_to_block(self.blocks[block_id]);
            if block_id == entry {
                self.emit_parameters();
            }
            for &statement in function.arena.items(block.statements) {
                self.emit_statement(statement);
            }
            self.emit_terminator(block.terminator);
        }
        self.builder.seal_all_blocks();
    }

    /// Binds the entry block's parameters: the result address of the program's entry, or each
    /// parameter's words to its local — the first locals of a function are its parameters.
    fn emit_parameters(&mut self) {
        let parameters = self
            .builder
            .block_params(self.blocks[self.function.shape.entry])
            .to_vec();
        if self.function.id == self.program.entry {
            self.result_address = Some(parameters[0]);
            return;
        }
        let parameter_words = (0..self.function.parameter_count)
            .map(|index| self.local_variables(LocalId::from_index(index)).len())
            .sum();
        for (index, &parameter) in parameters[..parameter_words].iter().enumerate() {
            self.builder.def_var(self.variables[index], parameter);
        }
    }

    fn emit_statement(&mut self, statement: NodeId) {
        match self.function.arena.operations[statement] {
            Operation::WriteLocal { local_id, value } => {
                let words = self.emit_value(value);
                for (index, &word) in words.as_slice().iter().enumerate() {
                    let variable = self.local_variables(local_id)[index];
                    self.builder.def_var(variable, word);
                }
            }
            Operation::WriteGlobal { global_id, value } => {
                let words = self.emit_value(value);
                let address = self.global_address(global_id);
                self.store_words(words, address);
            }
            Operation::Evaluate(value) => {
                self.emit_value(value);
            }
            Operation::ElementSet {
                array,
                indices,
                value,
                checked,
            } => self.emit_element_set(array, indices, value, checked),
            Operation::Store { .. } => unimplemented!("`Store` is not produced yet"),
            operation => unreachable!("{operation:?} is not a flat statement"),
        }
    }

    fn emit_terminator(&mut self, terminator: Terminator) {
        match terminator {
            Terminator::Goto(target) => {
                self.builder.ins().jump(self.blocks[target], &[]);
            }
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let cond = self.emit_value(cond).single();
                let (then_block, else_block) = (self.blocks[then_block], self.blocks[else_block]);
                self.builder
                    .ins()
                    .brif(cond, then_block, &[], else_block, &[]);
            }
            Terminator::Return(value) => {
                let words = match value {
                    Some(value) => self.emit_value(value),
                    None => Words::none(),
                };
                match self.result_address {
                    Some(address) => {
                        self.store_words(words, address);
                        self.builder.ins().return_(&[]);
                    }
                    None => {
                        self.builder.ins().return_(words.as_slice());
                    }
                }
            }
            Terminator::Switch { .. } => unimplemented!("`Switch` is not produced yet"),
            Terminator::RuntimeError(_) => unimplemented!("`RuntimeError` is not produced yet"),
        }
    }

    /// Raises the runtime error `message` when `failed` is true: the block ends with a branch to a
    /// cold block calling the runtime, and emission continues in a new block.
    fn emit_check(&mut self, failed: Value, message: &'static str) {
        let error_block = self.builder.create_block();
        let continue_block = self.builder.create_block();
        self.builder.set_cold_block(error_block);
        self.builder
            .ins()
            .brif(failed, error_block, &[], continue_block, &[]);
        self.builder.switch_to_block(error_block);
        let text = self.text_address(message.as_bytes());
        self.call_runtime("pinp_runtime_error", &[I64], &[], &[text]);
        // The runtime never returns: it jumps back to the host.
        let unreachable = TrapCode::user(1).expect("1 is a user trap code");
        self.builder.ins().trap(unreachable);
        self.builder.switch_to_block(continue_block);
    }

    fn local_variables(&self, local_id: LocalId) -> &[Variable] {
        let first = self.first_variable[local_id];
        let count = words(self.local_type(local_id)).len();
        &self.variables[first..first + count]
    }

    fn local_type(&self, local_id: LocalId) -> PinpType {
        let type_id = self.function.arena.locals[local_id].type_id;
        self.program.tables.types.get(type_id)
    }

    fn type_of(&self, node_id: NodeId) -> PinpType {
        let type_id = self.function.arena.types[node_id].expect("verify rejects an untyped node");
        self.program.tables.types.get(type_id)
    }

    fn global_address(&mut self, global_id: GlobalId) -> Value {
        let data_id = self.program.globals[global_id];
        self.data_address(data_id)
    }

    /// The address of `content` as NUL-terminated read-only data.
    fn text_address(&mut self, content: &[u8]) -> Value {
        let data_id = self.program.text(content);
        self.data_address(data_id)
    }

    fn data_address(&mut self, data_id: DataId) -> Value {
        let data_value = *self.data_values.entry(data_id).or_insert_with(|| {
            self.program
                .module
                .declare_data_in_func(data_id, self.builder.func)
        });
        self.builder.ins().symbol_value(I64, data_value)
    }

    /// Stores each word at its own 8-byte slot from `address`: how globals and the entry's result
    /// are laid out.
    fn store_words(&mut self, words: Words, address: Value) {
        for (index, &word) in words.as_slice().iter().enumerate() {
            let offset = (index * 8) as i32;
            self.builder
                .ins()
                .store(MemFlagsData::trusted(), word, address, offset);
        }
    }

    fn load_words(&mut self, pinp_type: PinpType, address: Value) -> Words {
        let mut loaded = Words::none();
        for (index, &word) in words(pinp_type).iter().enumerate() {
            let offset = (index * 8) as i32;
            loaded.values[index] =
                self.builder
                    .ins()
                    .load(word, MemFlagsData::trusted(), address, offset);
        }
        loaded.count = words(pinp_type).len();
        loaded
    }

    fn call_function(&mut self, func_id: FuncId, args: &[Value]) -> Words {
        let function_ref = *self.function_refs.entry(func_id).or_insert_with(|| {
            self.program
                .module
                .declare_func_in_func(func_id, self.builder.func)
        });
        let call = self.builder.ins().call(function_ref, args);
        Words::new(self.builder.inst_results(call))
    }

    /// Calls a function of the runtime library (or libc) with the given word types.
    fn call_runtime(
        &mut self,
        name: &'static str,
        params: &[Type],
        returns: &[Type],
        args: &[Value],
    ) -> Words {
        let func_id = self.program.runtime_function(name, params, returns);
        self.call_function(func_id, args)
    }
}
