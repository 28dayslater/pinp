// SPDX-License-Identifier: MIT

//! String ownership: where strings are copied and freed, spelled out as IR operations.
//!
//! A `str` owns heap storage, so every string must be freed exactly once. The model:
//!
//! * **Owned and borrowed.** A fresh string — a literal, a conversion, a concatenation, a copy, a
//!   call's result — is *owned*: whoever receives it must free it or hand it on. A read of a
//!   variable is *borrowed*: the variable still owns it.
//! * **Slots.** Every `str` variable and compiler temporary owns its value. Storing into one frees
//!   the value it held, and storing a borrowed string copies it, so no two slots share storage.
//!   A function frees its slots before it returns; the top level frees the globals too. A slot
//!   never written holds the empty string, whose free is harmless.
//! * **Consumers.** An operation that only reads its strings — a join, a comparison, a length —
//!   takes an owned operand through a temporary of its own (an *alias*), freed once the operation
//!   has run. A string discarded unread is freed at once.
//! * **Results.** A function's string result is moved out to the caller, a borrowed one copied
//!   first, since the slot it was read from is freed on the way out.
//!
//! Aliases are temporaries this module makes to move an owned string from where it is made to
//! where it is used: a read of one hands the string on, and none is freed at the end.

use super::*;
use crate::ir::RuntimeFunction;

impl Desugarer<'_> {
    /// Whether `node`, a string, hands over storage its receiver must free.
    pub(super) fn is_owned(&self, node: NodeId) -> bool {
        match self.function.arena.operations[node] {
            Operation::Constant(_) | Operation::Convert { .. } | Operation::Call { .. } => true,
            Operation::RuntimeCall { function, .. } => matches!(
                function,
                RuntimeFunction::StrConcat | RuntimeFunction::StrCopy
            ),
            Operation::ReadLocal(local_id) => self.function.aliases.contains(&local_id),
            _ => false,
        }
    }

    /// `value`, owned: a borrowed string is copied.
    fn owned(&mut self, value: NodeId) -> NodeId {
        if self.is_owned(value) {
            return value;
        }
        let span = self.function.arena.spans[value];
        let args = self.function.arena.list(&[value]);
        let operation = Operation::RuntimeCall {
            function: RuntimeFunction::StrCopy,
            args,
        };
        self.push(operation, PinpType::Str, span)
    }

    /// The statement freeing `value`.
    fn free(&mut self, value: NodeId, span: Span) -> NodeId {
        let args = self.function.arena.list(&[value]);
        let operation = Operation::RuntimeCall {
            function: RuntimeFunction::StrFree,
            args,
        };
        let call = self.push(operation, PinpType::Void, span);
        self.statement_node(Operation::Evaluate(call), span)
    }

    /// Moves the owned `value` into a new alias, written in `out`; returns a read of it.
    fn alias(&mut self, value: NodeId, span: Span, out: &mut Vec<NodeId>) -> NodeId {
        let alias = self.temporary(PinpType::Str);
        self.function.aliases.insert(alias);
        out.push(self.write_local(alias, value, span));
        self.read_local(alias, span)
    }

    /// Stores the string `value` in `binding`: the old value freed, a borrowed one copied. The new
    /// value is computed first when it reads the binding or calls a function, which might free or
    /// replace what the binding holds.
    pub(super) fn store_string(
        &mut self,
        binding: Binding,
        value: NodeId,
        span: Span,
        out: &mut Vec<NodeId>,
    ) {
        let mut value = self.owned(value);
        if self.reads_binding(value, binding) || self.calls_a_function(value) {
            value = self.alias(value, span, out);
        }
        let old = self.read(binding, span);
        out.push(self.free(old, span));
        let operation = match binding {
            Binding::Local(local_id) => Operation::WriteLocal { local_id, value },
            Binding::Global(global_id) => Operation::WriteGlobal { global_id, value },
        };
        out.push(self.statement_node(operation, span));
    }

    /// Discards `value`: an owned string is freed, anything else evaluated for its effect.
    pub(super) fn discard(&mut self, value: NodeId, span: Span, out: &mut Vec<NodeId>) {
        let statement = match self.node_type(value) == PinpType::Str && self.is_owned(value) {
            true => self.free(value, span),
            false => self.statement_node(Operation::Evaluate(value), span),
        };
        out.push(statement);
    }

    /// Builds `make(operands)` for an operation that only reads its string operands. Owned ones
    /// are moved into aliases first and freed once it has run — and so is every borrowed one
    /// before an operand that calls a function, which could otherwise free it first. The result
    /// is then computed ahead of the statement too, into a temporary of its own.
    pub(super) fn consume(
        &mut self,
        mut operands: Vec<NodeId>,
        result_type: PinpType,
        span: Span,
        out: &mut Vec<NodeId>,
        make: impl FnOnce(&mut Self, &[NodeId]) -> Operation,
    ) -> NodeId {
        let owned: Vec<bool> = operands
            .iter()
            .map(|&operand| self.node_type(operand) == PinpType::Str && self.is_owned(operand))
            .collect();
        let Some(last_owned) = owned.iter().rposition(|&owned| owned) else {
            let operation = make(self, &operands);
            return self.push(operation, result_type, span);
        };
        let mut to_free = Vec::new();
        for index in 0..=last_owned {
            let operand = operands[index];
            if self.node_type(operand) != PinpType::Str {
                continue;
            }
            let call_follows = operands[index + 1..=last_owned]
                .iter()
                .any(|&later| self.calls_a_function(later));
            if owned[index] || call_follows {
                let operand_span = self.function.arena.spans[operand];
                let value = self.owned(operand);
                if !self.is_alias_read(value) {
                    operands[index] = self.alias(value, operand_span, out);
                }
                to_free.push((operands[index], operand_span));
            }
        }
        let operation = make(self, &operands);
        let consumed = self.push(operation, result_type, span);
        let result = match result_type {
            PinpType::Str => self.alias(consumed, span, out),
            _ => {
                let temporary = self.temporary(result_type);
                out.push(self.write_local(temporary, consumed, span));
                self.read_local(temporary, span)
            }
        };
        for (alias_read, alias_span) in to_free {
            let again = self.reread_local(alias_read);
            out.push(self.free(again, alias_span));
        }
        result
    }

    /// Frees every string the function owns — its slots, and at the top level its globals —
    /// after computing `result`, which the frees must not reach. Returns the result to return:
    /// a string moved out, borrowed ones copied first.
    pub(super) fn free_on_exit(
        &mut self,
        result: Option<NodeId>,
        end_span: Span,
        out: &mut Vec<NodeId>,
    ) -> Option<NodeId> {
        let mut slots: Vec<Binding> = Vec::new();
        for (local_id, local) in self.function.arena.locals.iter() {
            let is_string = self.tables.types.get(local.type_id) == PinpType::Str;
            if is_string && !self.function.aliases.contains(&local_id) {
                slots.push(Binding::Local(local_id));
            }
        }
        if self.function.is_entry {
            for (global_id, global) in self.tables.globals.iter() {
                if self.tables.types.get(global.type_id) == PinpType::Str {
                    slots.push(Binding::Global(global_id));
                }
            }
        }
        let result = result.map(|result| {
            let span = self.function.arena.spans[result];
            match self.node_type(result) {
                PinpType::Str => {
                    let value = self.owned(result);
                    match slots.is_empty() || self.is_alias_read(value) {
                        true => value,
                        false => self.alias(value, span, out),
                    }
                }
                _ if slots.is_empty() || self.is_untouched_by_frees(result) => result,
                result_type => {
                    let temporary = self.temporary(result_type);
                    out.push(self.write_local(temporary, result, span));
                    self.read_local(temporary, span)
                }
            }
        });
        for slot in slots {
            let old = self.read(slot, end_span);
            out.push(self.free(old, end_span));
        }
        result
    }

    fn is_alias_read(&self, node: NodeId) -> bool {
        matches!(self.function.arena.operations[node],
            Operation::ReadLocal(local_id) if self.function.aliases.contains(&local_id))
    }

    /// Whether a result can be computed after the frees: a constant, or a read of a variable that
    /// is not a string.
    fn is_untouched_by_frees(&self, node: NodeId) -> bool {
        matches!(
            self.function.arena.operations[node],
            Operation::Constant(_) | Operation::ReadLocal(_) | Operation::ReadGlobal(_)
        )
    }

    /// Another node reading the local `read` reads.
    fn reread_local(&mut self, read: NodeId) -> NodeId {
        let Operation::ReadLocal(local_id) = self.function.arena.operations[read] else {
            unreachable!("an alias is read through `ReadLocal`")
        };
        let span = self.function.arena.spans[read];
        self.read_local(local_id, span)
    }

    fn reads_binding(&self, root: NodeId, binding: Binding) -> bool {
        let arena = &self.function.arena;
        let mut found = false;
        arena.for_each_in_tree(root, |node| {
            found |= match (arena.operations[node], binding) {
                (Operation::ReadLocal(read), Binding::Local(local_id)) => read == local_id,
                (Operation::ReadGlobal(read), Binding::Global(global_id)) => read == global_id,
                _ => false,
            };
        });
        found
    }

    fn calls_a_function(&self, root: NodeId) -> bool {
        let arena = &self.function.arena;
        let mut found = false;
        arena.for_each_in_tree(root, |node| {
            found |= matches!(arena.operations[node], Operation::Call { .. });
        });
        found
    }
}
