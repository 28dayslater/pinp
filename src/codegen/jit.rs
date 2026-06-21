// SPDX-License-Identifier: MIT

use std::ffi::{CStr, CString};

use inkwell::llvm_sys::error::{LLVMDisposeErrorMessage, LLVMErrorRef, LLVMGetErrorMessage};
use inkwell::llvm_sys::orc2::lljit::{
    LLVMOrcCreateLLJIT, LLVMOrcDisposeLLJIT, LLVMOrcLLJITAddLLVMIRModule,
    LLVMOrcLLJITGetGlobalPrefix, LLVMOrcLLJITGetMainJITDylib, LLVMOrcLLJITLookup, LLVMOrcLLJITRef,
};
use inkwell::llvm_sys::orc2::{
    LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess, LLVMOrcCreateNewThreadSafeContext,
    LLVMOrcCreateNewThreadSafeModule, LLVMOrcDefinitionGeneratorRef,
    LLVMOrcDisposeThreadSafeContext, LLVMOrcExecutorAddress, LLVMOrcJITDylibAddGenerator,
};
use inkwell::module::Module;
use inkwell::targets::{InitializationConfig, Target};

/// A minimal ORCv2 LLJIT instance. Owns the underlying `LLVMOrcLLJITRef` and
/// disposes it on drop.
///
/// The inkwell `Context` a module was built in must outlive the `Jit` it was
/// added to: ORC frees the module on dispose, which must precede its context.
pub struct Jit {
    jit: LLVMOrcLLJITRef,
}

impl Jit {
    /// Creates a JIT for the host machine.
    pub fn new() -> Result<Self, String> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(|error| format!("Failed to initialize the native target: {error}."))?;

        let mut jit: LLVMOrcLLJITRef = std::ptr::null_mut();
        // SAFETY: a null builder asks ORC for a default LLJIT; on success it writes
        // the instance into `jit`, otherwise it returns a non-null error.
        unsafe { check_error(LLVMOrcCreateLLJIT(&mut jit, std::ptr::null_mut()))? };

        // Let the main dylib resolve symbols against the host process. Symbols that
        // JIT-compiled pinp code references but does not define are satisfied this way:
        // the `pinp_*` runtime allocator linked into the binary (build.rs), and host
        // libc as later features need it.
        // SAFETY: `jit` is the LLJIT just created; the generator is built with the JIT's
        // own global-symbol prefix and a null filter (admit every symbol), then handed to
        // the main dylib, which takes ownership of it.
        unsafe {
            let global_prefix = LLVMOrcLLJITGetGlobalPrefix(jit);
            let mut generator: LLVMOrcDefinitionGeneratorRef = std::ptr::null_mut();
            check_error(LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess(
                &mut generator,
                global_prefix,
                None,
                std::ptr::null_mut(),
            ))?;
            LLVMOrcJITDylibAddGenerator(LLVMOrcLLJITGetMainJITDylib(jit), generator);
        }

        Ok(Jit { jit })
    }

    /// Adds an inkwell module to the JIT, transferring ownership of the module to
    /// ORC. The module's `Context` must outlive this `Jit` (see the type docs).
    pub fn add_module(&self, module: Module) -> Result<(), String> {
        let module_ref = module.as_mut_ptr();
        // ORC takes ownership of the raw module below, so release inkwell's RAII
        // handle without disposing it.
        std::mem::forget(module);

        // SAFETY: `module_ref` is a valid, now inkwell-unowned module. The throwaway
        // ThreadSafeContext only guards ORC's (single-threaded) locking; the module
        // keeps its real context. `add` consumes the ThreadSafeModule.
        unsafe {
            let thread_safe_context = LLVMOrcCreateNewThreadSafeContext();
            let thread_safe_module =
                LLVMOrcCreateNewThreadSafeModule(module_ref, thread_safe_context);
            LLVMOrcDisposeThreadSafeContext(thread_safe_context);

            let main_dylib = LLVMOrcLLJITGetMainJITDylib(self.jit);
            check_error(LLVMOrcLLJITAddLLVMIRModule(
                self.jit,
                main_dylib,
                thread_safe_module,
            ))
        }
    }

    /// Looks up a JITed symbol and returns it as the function-pointer type `FPT`.
    ///
    /// # Safety
    ///
    /// `FPT` must be a function-pointer type whose signature matches the symbol's
    /// actual ABI, or calling it is undefined behaviour.
    pub unsafe fn lookup<FPT: Copy>(&self, name: &str) -> Result<FPT, String> {
        let symbol = CString::new(name).map_err(|error| error.to_string())?;
        let mut address: LLVMOrcExecutorAddress = 0;
        // SAFETY: the caller guarantees `FPT` matches the symbol's ABI.
        unsafe {
            check_error(LLVMOrcLLJITLookup(self.jit, &mut address, symbol.as_ptr()))?;
            // `address` is the symbol's runtime address; reinterpret it as `FPT`.
            Ok(std::mem::transmute_copy::<LLVMOrcExecutorAddress, FPT>(
                &address,
            ))
        }
    }
}

impl Drop for Jit {
    fn drop(&mut self) {
        // SAFETY: `self.jit` was created in `new` and is disposed exactly once.
        unsafe {
            LLVMOrcDisposeLLJIT(self.jit);
        }
    }
}

/// Turns an `LLVMErrorRef` into a `Result`: null is success, otherwise the error's
/// message is extracted and the error consumed.
unsafe fn check_error(error: LLVMErrorRef) -> Result<(), String> {
    if error.is_null() {
        return Ok(());
    }
    // SAFETY: `error` is non-null here, so the caller's LLVM error pointer is live.
    unsafe {
        let message = LLVMGetErrorMessage(error); // consumes `error`, returns owned C string
        let owned = CStr::from_ptr(message).to_string_lossy().into_owned();
        LLVMDisposeErrorMessage(message);
        Err(owned)
    }
}
