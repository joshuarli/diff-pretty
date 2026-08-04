//! Needleman-Wunsch / Wagner-Fischer alignment, replicated exactly from delta's
//! `src/align.rs` (costs, parent pointers, tie-breaking, and operation order all
//! matter for byte-for-byte output).

const DELETION_COST: usize = 2;
const INSERTION_COST: usize = 2;
const INITIAL_MISMATCH_PENALTY: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    NoOp,
    Deletion,
    Insertion,
}

use Operation::*;

#[derive(Clone, Copy, Debug)]
struct Cell {
    parent: usize,
    operation: Operation,
    cost: usize,
}

pub struct Alignment<'a> {
    pub x: Vec<&'a str>,
    pub y: Vec<&'a str>,
    table: Vec<Cell>,
    dim: [usize; 2],
}

impl<'a> Alignment<'a> {
    pub fn new(x: Vec<&'a str>, y: Vec<&'a str>) -> Self {
        let mut alignment = Self {
            x,
            y,
            table: Vec::new(),
            dim: [0, 0],
        };
        alignment.reset_buffers();
        alignment
    }

    /// Re-run the alignment on a new minus/plus line pair, tokenizing the lines
    /// directly into the reused `x`/`y` buffers and reusing the table when it is
    /// large enough. Word-diff infers candidate pairings by running this
    /// repeatedly, so per-candidate allocations are avoided entirely.
    pub fn reset_lines(&mut self, minus_line: &'a str, plus_line: &'a str) {
        crate::edits::tokenize_into(minus_line, &mut self.x);
        crate::edits::tokenize_into(plus_line, &mut self.y);
        self.reset_buffers();
    }

    fn reset_buffers(&mut self) {
        self.dim = [self.y.len() + 1, self.x.len() + 1];
        let size = self.dim[0] * self.dim[1];
        if self.table.len() < size {
            self.table = vec![
                Cell {
                    parent: 0,
                    operation: NoOp,
                    cost: 0,
                };
                size
            ];
        } else {
            self.table.truncate(size);
        }
        self.fill();
    }

    fn fill(&mut self) {
        for i in 1..self.dim[1] {
            self.table[i] = Cell {
                parent: 0,
                operation: Deletion,
                cost: i * DELETION_COST + INITIAL_MISMATCH_PENALTY,
            };
        }
        for j in 1..self.dim[0] {
            self.table[j * self.dim[1]] = Cell {
                parent: 0,
                operation: Insertion,
                cost: j * INSERTION_COST + INITIAL_MISMATCH_PENALTY,
            };
        }
        for (i, x_i) in self.x.iter().enumerate() {
            for (j, y_j) in self.y.iter().enumerate() {
                let (left, diag, up) = (
                    self.index(i, j + 1),
                    self.index(i, j),
                    self.index(i + 1, j),
                );
                let candidates = [
                    Cell {
                        parent: up,
                        operation: Insertion,
                        cost: self.mismatch_cost(up, INSERTION_COST),
                    },
                    Cell {
                        parent: left,
                        operation: Deletion,
                        cost: self.mismatch_cost(left, DELETION_COST),
                    },
                    Cell {
                        parent: diag,
                        operation: NoOp,
                        cost: if x_i == y_j {
                            self.table[diag].cost
                        } else {
                            usize::MAX
                        },
                    },
                ];
                let index = self.index(i + 1, j + 1);
                self.table[index] = candidates
                    .iter()
                    .min_by_key(|cell| cell.cost)
                    .unwrap()
                    .clone();
            }
        }
    }

    fn mismatch_cost(&self, parent: usize, basic_cost: usize) -> usize {
        self.table[parent].cost
            + basic_cost
            + if self.table[parent].operation == NoOp {
                INITIAL_MISMATCH_PENALTY
            } else {
                0
            }
    }

    /// Run-length encode the backtrace into `encoded` in a single pass, without
    /// building an intermediate `Vec<Operation>` and without allocating (the
    /// buffer is cleared and reused by the caller). The backtrace is walked
    /// bottom-right to top-left; runs are pushed in reverse, then flipped.
    pub fn coalesced_operations_into(&self, encoded: &mut Vec<(Operation, usize)>) {
        encoded.clear();
        let mut p = self.index(self.x.len(), self.y.len());
        let mut run: usize = 0;
        let mut curr_op = Operation::NoOp;
        loop {
            let cell = &self.table[p];
            if run == 0 {
                curr_op = cell.operation;
                run = 1;
            } else if cell.operation == curr_op {
                run += 1;
            } else {
                encoded.push((curr_op, run));
                curr_op = cell.operation;
                run = 1;
            }
            if cell.parent == 0 {
                break;
            }
            p = cell.parent;
        }
        encoded.push((curr_op, run));
        encoded.reverse();
    }

    fn index(&self, i: usize, j: usize) -> usize {
        j * self.dim[1] + i
    }
}
