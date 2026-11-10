// SPDX-License-Identifier: MIT

//! Arrays and matrices: allocation, element access, and slices. An array is a pointer to its
//! elements, stored row by row for a matrix — one byte per `bool`, eight per `int` or `float`.

use cranelift_codegen::ir::condcodes::IntCC;

use super::*;
use crate::ir::List;
use crate::parser::ArrayElementType;

const OUT_OF_BOUNDS: &str = "Array index out of bounds.";

/// The element type and the length of each dimension of an array or matrix type.
fn shape(array_type: PinpType) -> (ArrayElementType, Vec<usize>) {
    match array_type {
        PinpType::Array(element, length) => (element, vec![length]),
        PinpType::Matrix(element, rows, columns) => (element, vec![rows, columns]),
        other => unreachable!("{other:?} is not an array"),
    }
}

fn element_word(element: ArrayElementType) -> Type {
    match element {
        ArrayElementType::Bool => I8,
        ArrayElementType::Int => I64,
        ArrayElementType::Float => F64,
    }
}

fn element_size(element: ArrayElementType) -> i64 {
    match element {
        ArrayElementType::Bool => 1,
        ArrayElementType::Int | ArrayElementType::Float => 8,
    }
}

/// How a slice picks along one dimension.
#[derive(Clone, Copy)]
enum Selector {
    /// Every value of a range: its start and step.
    Range { start: Value, step: Value },
    /// One index.
    Index(Value),
}

impl FunctionEmitter<'_, '_> {
    /// A new array of `array_type` holding `elements`, already of its element type.
    pub(super) fn emit_array_new(&mut self, elements: List<NodeId>, array_type: PinpType) -> Words {
        let (element, _) = shape(array_type);
        let array = self.emit_array_allocate(array_type, false);
        let function = self.function;
        for (position, &node) in function.arena.items(elements).iter().enumerate() {
            let value = self.emit_value(node).single();
            let offset = (position as i64 * element_size(element)) as i32;
            self.builder
                .ins()
                .store(MemFlagsData::trusted(), value, array, offset);
        }
        Words::one(array)
    }

    /// Storage for an array of `array_type`, its elements zeroed or left unset.
    pub(super) fn emit_array_allocate(&mut self, array_type: PinpType, zeroed: bool) -> Value {
        let (element, dimensions) = shape(array_type);
        let bytes = dimensions.iter().product::<usize>() as i64 * element_size(element);
        let size = self.builder.ins().iconst(I64, bytes);
        let array = self
            .call_runtime("pinp_alloc", &[I64], &[I64], &[size])
            .single();
        if zeroed {
            let zero = self.builder.ins().iconst(I8, 0);
            let target = self.program.module.target_config();
            self.builder.call_memset(target, array, zero, size);
        }
        array
    }

    pub(super) fn emit_element_get(
        &mut self,
        array: NodeId,
        indices: List<NodeId>,
        checked: bool,
        element_type: PinpType,
    ) -> Words {
        let array_type = self.type_of(array);
        let array = self.emit_value(array).single();
        let indices = self.emit_arguments(indices);
        let address = self.element_address(array_type, array, &indices, checked);
        let word = words(element_type)[0];
        Words::one(
            self.builder
                .ins()
                .load(word, MemFlagsData::trusted(), address, 0),
        )
    }

    pub(super) fn emit_element_set(
        &mut self,
        array: NodeId,
        indices: List<NodeId>,
        value: NodeId,
        checked: bool,
    ) {
        let array_type = self.type_of(array);
        let array = self.emit_value(array).single();
        let indices = self.emit_arguments(indices);
        let value = self.emit_value(value).single();
        let address = self.element_address(array_type, array, &indices, checked);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), value, address, 0);
    }

    /// The address of the element at `indices`, each checked against its dimension when `checked`.
    fn element_address(
        &mut self,
        array_type: PinpType,
        array: Value,
        indices: &[Value],
        checked: bool,
    ) -> Value {
        let (element, dimensions) = shape(array_type);
        if checked {
            for (&index, &dimension) in indices.iter().zip(&dimensions) {
                self.check_index(index, dimension);
            }
        }
        let position = self.flat_position(indices, &dimensions);
        self.position_address(array, position, element)
    }

    /// Raises "Array index out of bounds." unless `0 <= index < dimension`: compared unsigned, a
    /// negative index is past every dimension.
    fn check_index(&mut self, index: Value, dimension: usize) {
        let outside = self.builder.ins().icmp_imm_s(
            IntCC::UnsignedGreaterThanOrEqual,
            index,
            dimension as i64,
        );
        self.emit_check(outside, OUT_OF_BOUNDS);
    }

    /// The row-major position of `indices`: the index itself, or `row * columns + column`.
    fn flat_position(&mut self, indices: &[Value], dimensions: &[usize]) -> Value {
        match indices {
            [index] => *index,
            [row, column] => {
                let row_start = self.builder.ins().imul_imm_s(*row, dimensions[1] as i64);
                self.builder.ins().iadd(row_start, *column)
            }
            _ => unreachable!("an array has one or two dimensions"),
        }
    }

    fn position_address(
        &mut self,
        array: Value,
        position: Value,
        element: ArrayElementType,
    ) -> Value {
        let offset = self
            .builder
            .ins()
            .imul_imm_s(position, element_size(element));
        self.builder.ins().iadd(array, offset)
    }

    /// A slice: a new array of `result_type` copied from `source`, a range selector taking every
    /// value of its range along its dimension and an index selector one row or column.
    pub(super) fn emit_slice(
        &mut self,
        source: NodeId,
        selectors: List<NodeId>,
        result_type: PinpType,
    ) -> Words {
        let source_type = self.type_of(source);
        let (element, source_dimensions) = shape(source_type);
        let source = self.emit_value(source).single();
        let function = self.function;
        let mut picks = Vec::new();
        for &selector in function.arena.items(selectors) {
            let selected = self.emit_value(selector);
            picks.push(match self.type_of(selector) {
                PinpType::Range => Selector::Range {
                    start: selected.values[0],
                    step: selected.values[2],
                },
                _ => Selector::Index(selected.single()),
            });
        }
        for (pick, &dimension) in picks.iter().zip(&source_dimensions) {
            if let Selector::Index(index) = *pick {
                self.check_index(index, dimension);
            }
        }

        let (_, result_dimensions) = shape(result_type);
        let count: usize = result_dimensions.iter().product();
        let result = self.emit_array_allocate(result_type, false);
        let header = self.builder.create_block();
        let body = self.builder.create_block();
        let exit = self.builder.create_block();
        let position = self.builder.append_block_param(header, I64);
        let zero = self.builder.ins().iconst(I64, 0);
        self.builder.ins().jump(header, &[zero.into()]);

        self.builder.switch_to_block(header);
        let more = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::SignedLessThan, position, count as i64);
        self.builder.ins().brif(more, body, &[], exit, &[]);

        self.builder.switch_to_block(body);
        // The position along each range selector's dimension of the result, row-major.
        let along = match result_dimensions[..] {
            [_] => vec![position],
            [_, columns] => vec![
                self.builder.ins().udiv_imm_u(position, columns as i64),
                self.builder.ins().urem_imm_u(position, columns as i64),
            ],
            _ => unreachable!("a slice has one or two dimensions"),
        };
        let mut along = along.into_iter();
        let mut source_indices = Vec::with_capacity(picks.len());
        for pick in &picks {
            source_indices.push(match *pick {
                Selector::Range { start, step } => {
                    let offset = self
                        .builder
                        .ins()
                        .imul(along.next().expect("a result dimension per range"), step);
                    self.builder.ins().iadd(start, offset)
                }
                Selector::Index(index) => index,
            });
        }
        let source_position = self.flat_position(&source_indices, &source_dimensions);
        let from = self.position_address(source, source_position, element);
        let value =
            self.builder
                .ins()
                .load(element_word(element), MemFlagsData::trusted(), from, 0);
        let to = self.position_address(result, position, element);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), value, to, 0);
        let next = self.builder.ins().iadd_imm_s(position, 1);
        self.builder.ins().jump(header, &[next.into()]);

        self.builder.switch_to_block(exit);
        Words::one(result)
    }
}
