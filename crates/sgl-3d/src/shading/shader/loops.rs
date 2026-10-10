//! AR-12 for a game's module (specs/sgl3d-architecture.md, Rules): every
//! loop is a counted loop, whose count its literal or `const` start, limit
//! and step fix whatever the data, and one call of each function makes at
//! most `SHADER_LOOP_BUDGET` iterations. A loop is counted when its counter,
//! a function-local `i32` or `u32`:
//!
//! - is tested before each iteration against a constant limit, as `for`
//!   writes it (`if i < N {} else { break; }` first in the body, `<` or
//!   `<=`), or after it in `break if` (`>=` or `>`);
//! - is changed by exactly one statement outside its initialisation, in the
//!   loop's `continuing` block, adding a positive constant step to it;
//! - starts at a constant: its declaration's, or a store just before the
//!   loop in the same block, as naga writes a `for` nested in another loop
//!   (a `break if` loop within another loop needs the store);
//! - is read and written nowhere else but through loads, and never passed
//!   as a pointer;
//! - is read by the test and the step where they stand: naga evaluates an
//!   expression, loads included, at the emit that covers it, so both must be
//!   emitted just before the `if` (or after the step, for `break if`) and
//!   just before the store, not by a `let` earlier, as before the loop;
//! - and its limit plus its step fit its type, so it cannot wrap.
//!
//! Anything else is refused, whatever it would do. The iterations of a call
//! are counted as a nest of loops runs them: a loop's count times its body's
//! (at least one), one statement after another's added, a branch's larger
//! arm, a switch's cases added and a call's callee's (WGSL has no recursion).
use crate::content::shader::{SHADER_LOOP_BUDGET, ShaderError};
use naga::{BinaryOperator, Block, Expression, Function, Handle, Literal, Module, Statement};
use std::collections::HashMap;

/// Checks every function of `module` whose name `game` holds for.
pub(super) fn check(module: &Module, game: impl Fn(&str) -> bool) -> Result<(), ShaderError> {
    let mut counter = Counter {
        module,
        counted: HashMap::new(),
    };
    for (handle, function) in module.functions.iter() {
        let name = function.name.clone().unwrap_or_default();
        if !game(&name) {
            continue;
        }
        let iterations = counter
            .function(handle)
            .ok_or_else(|| ShaderError::UnboundedLoop {
                function: name.clone(),
            })?;
        if iterations > SHADER_LOOP_BUDGET {
            return Err(ShaderError::LoopBudget {
                function: name,
                iterations,
            });
        }
    }
    Ok(())
}

/// Each function's iterations a call makes, none where a loop is not
/// counted, as they are found.
struct Counter<'a> {
    module: &'a Module,
    counted: HashMap<Handle<Function>, Option<u64>>,
}

impl Counter<'_> {
    fn function(&mut self, handle: Handle<Function>) -> Option<u64> {
        if let Some(&iterations) = self.counted.get(&handle) {
            return iterations;
        }
        let function = &self.module.functions[handle];
        let iterations = self.block(function, &function.body, false);
        self.counted.insert(handle, iterations);
        iterations
    }

    /// The iterations `block` makes; `nested` where a loop holds it, which
    /// may run it again.
    fn block(&mut self, function: &Function, block: &Block, nested: bool) -> Option<u64> {
        let mut total = 0u64;
        for (at, statement) in block.iter().enumerate() {
            let iterations = match statement {
                Statement::Block(inner) => self.block(function, inner, nested)?,
                Statement::If { accept, reject, .. } => self
                    .block(function, accept, nested)?
                    .max(self.block(function, reject, nested)?),
                Statement::Switch { cases, .. } => {
                    let mut sum = 0u64;
                    for case in cases {
                        sum = sum.saturating_add(self.block(function, &case.body, nested)?);
                    }
                    sum
                }
                Statement::Loop {
                    body,
                    continuing,
                    break_if,
                } => {
                    let count = counted(
                        self.module,
                        function,
                        (block, at),
                        body,
                        continuing,
                        *break_if,
                        nested,
                    )?;
                    let inner = self
                        .block(function, body, true)?
                        .saturating_add(self.block(function, continuing, true)?);
                    count.saturating_mul(inner.max(1))
                }
                Statement::Call {
                    function: callee, ..
                } => self.function(*callee)?,
                _ => 0,
            };
            total = total.saturating_add(iterations);
        }
        Some(total)
    }
}

/// The value of `expression`, a literal or a constant's, as an integer.
fn constant(module: &Module, function: &Function, expression: Handle<Expression>) -> Option<i64> {
    let literal = |literal: &Literal| match *literal {
        Literal::I32(value) => Some(i64::from(value)),
        Literal::U32(value) => Some(i64::from(value)),
        Literal::AbstractInt(value) => Some(value),
        _ => None,
    };
    match &function.expressions[expression] {
        Expression::Literal(value) => literal(value),
        Expression::ZeroValue(_) => Some(0),
        Expression::Constant(handle) => {
            match &module.global_expressions[module.constants[*handle].init] {
                Expression::Literal(value) => literal(value),
                Expression::ZeroValue(_) => Some(0),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The local variable `pointer` names, if it names one whole.
fn local(function: &Function, pointer: Handle<Expression>) -> Option<Handle<naga::LocalVariable>> {
    match function.expressions[pointer] {
        Expression::LocalVariable(variable) => Some(variable),
        _ => None,
    }
}

/// The local variable `expression` loads, if it loads one whole.
fn loaded(
    function: &Function,
    expression: Handle<Expression>,
) -> Option<Handle<naga::LocalVariable>> {
    match function.expressions[expression] {
        Expression::Load { pointer } => local(function, pointer),
        _ => None,
    }
}

/// A test of a counter against a constant: the counter, the operator with
/// the counter on its left, and the limit.
fn comparison(
    module: &Module,
    function: &Function,
    test: Handle<Expression>,
) -> Option<(Handle<naga::LocalVariable>, BinaryOperator, i64)> {
    let Expression::Binary { op, left, right } = function.expressions[test] else {
        return None;
    };
    if let Some(variable) = loaded(function, left) {
        return Some((variable, op, constant(module, function, right)?));
    }
    let variable = loaded(function, right)?;
    let mirrored = match op {
        BinaryOperator::Less => BinaryOperator::Greater,
        BinaryOperator::LessEqual => BinaryOperator::GreaterEqual,
        BinaryOperator::Greater => BinaryOperator::Less,
        BinaryOperator::GreaterEqual => BinaryOperator::LessEqual,
        _ => return None,
    };
    Some((variable, mirrored, constant(module, function, left)?))
}

/// The iterations the loop at `at` in `block` makes, where it is counted;
/// `nested` where an enclosing loop may run it again.
fn counted(
    module: &Module,
    function: &Function,
    (block, at): (&Block, usize),
    body: &Block,
    continuing: &Block,
    break_if: Option<Handle<Expression>>,
    nested: bool,
) -> Option<u64> {
    // The test: the body's first statement but emits, `if test {} else
    // { break; }`, which runs while it holds; or `break if`, after each
    // iteration, which stops once it holds.
    let (variable, op, limit, before) = match break_if {
        Some(test) => {
            let (variable, op, limit) = comparison(module, function, test)?;
            (variable, op, limit, false)
        }
        None => {
            let at = body
                .iter()
                .position(|statement| !matches!(statement, Statement::Emit(_)))?;
            let Statement::If {
                condition,
                accept,
                reject,
            } = &body[at]
            else {
                return None;
            };
            if !accept.is_empty() || reject.len() != 1 || !matches!(reject[0], Statement::Break) {
                return None;
            }
            let (variable, op, limit) = comparison(module, function, *condition)?;
            // Evaluated by the emits before it, each iteration.
            if !evaluated_in(function, &body[..at], *condition) {
                return None;
            }
            (variable, op, limit, true)
        }
    };
    let (low, high) = match module.types[function.local_variables[variable].ty].inner {
        naga::TypeInner::Scalar(naga::Scalar {
            kind: naga::ScalarKind::Sint,
            width: 4,
        }) => (i64::from(i32::MIN), i64::from(i32::MAX)),
        naga::TypeInner::Scalar(naga::Scalar {
            kind: naga::ScalarKind::Uint,
            width: 4,
        }) => (0, i64::from(u32::MAX)),
        _ => return None,
    };
    // The step: the one store to the counter in `continuing`, of the counter
    // plus a positive constant.
    let mut steps = continuing
        .iter()
        .enumerate()
        .filter_map(|(at, statement)| match statement {
            Statement::Store { pointer, value } if local(function, *pointer) == Some(variable) => {
                Some((at, *value))
            }
            _ => None,
        });
    let ((store_at, step), None) = (steps.next()?, steps.next()) else {
        return None;
    };
    // The step reads the counter just before its store, and `break if` just
    // after it, each iteration: a `let` computed elsewhere, as before the
    // loop, holds one value for ever (#287).
    if !evaluated_in(function, &continuing[..store_at], step)
        || break_if.is_some_and(|test| !evaluated_in(function, &continuing[store_at + 1..], test))
    {
        return None;
    }
    let Expression::Binary {
        op: BinaryOperator::Add,
        left,
        right,
    } = function.expressions[step]
    else {
        return None;
    };
    let step = if loaded(function, left) == Some(variable) {
        constant(module, function, right)?
    } else if loaded(function, right) == Some(variable) {
        constant(module, function, left)?
    } else {
        return None;
    };
    // The start: a store just before the loop in its block, else the
    // declaration's.
    let start_store = block[..at]
        .iter()
        .rev()
        .find(|statement| !matches!(statement, Statement::Emit(_)))
        .and_then(|statement| match statement {
            Statement::Store { pointer, value } if local(function, *pointer) == Some(variable) => {
                Some(*value)
            }
            _ => None,
        });
    // A `break if` loop runs once more whatever its counter holds, so one an
    // enclosing loop runs again from where it left off steps on past its
    // limit each time, until the counter wraps: it starts again only from a
    // store before it.
    if start_store.is_none() && !before && nested {
        return None;
    }
    let start = match start_store {
        Some(value) => constant(module, function, value)?,
        None => match function.local_variables[variable].init {
            Some(init) => constant(module, function, init)?,
            None => 0,
        },
    };
    // Every other use of the counter's pointer: none.
    if stores(function, &function.body, variable) != 1 + usize::from(start_store.is_some())
        || pointer_escapes(function, variable)
    {
        return None;
    }
    let fits = |value: i64| (low..=high).contains(&value);
    if step <= 0 || !fits(start) || !fits(limit) || !fits(step) || !fits(limit + step) {
        return None;
    }
    // Iterations: the count the counter takes from the start in steps while
    // the test passes; after it, at least one.
    let (span, step) = (limit - start, step as u64);
    let ahead = u64::try_from(span).unwrap_or(0);
    let iterations = match (before, op) {
        (true, BinaryOperator::Less) => ahead.div_ceil(step),
        (true, BinaryOperator::LessEqual) if span >= 0 => ahead / step + 1,
        (true, BinaryOperator::LessEqual) => 0,
        (false, BinaryOperator::GreaterEqual) => ahead.div_ceil(step).max(1),
        (false, BinaryOperator::Greater) if span >= 0 => ahead / step + 1,
        (false, BinaryOperator::Greater) => 1,
        _ => return None,
    };
    Some(iterations)
}

/// Whether the emits of `statements` evaluate `expression`, a binary
/// operation, and every load it operates on. Naga evaluates an expression,
/// and reads the variable a load names, at the emit that covers it, however
/// often later statements refer to it.
fn evaluated_in(
    function: &Function,
    statements: &[Statement],
    expression: Handle<Expression>,
) -> bool {
    let emitted = |handle: Handle<Expression>| {
        statements.iter().any(|statement| {
            matches!(statement, Statement::Emit(range) if range.clone().any(|emitted| emitted == handle))
        })
    };
    let Expression::Binary { left, right, .. } = function.expressions[expression] else {
        return false;
    };
    emitted(expression)
        && [left, right].into_iter().all(|operand| {
            !matches!(function.expressions[operand], Expression::Load { .. }) || emitted(operand)
        })
}

/// The stores to `variable` in `block` and everything it holds.
fn stores(function: &Function, block: &Block, variable: Handle<naga::LocalVariable>) -> usize {
    block
        .iter()
        .map(|statement| match statement {
            Statement::Store { pointer, .. } => {
                usize::from(local(function, *pointer) == Some(variable))
            }
            Statement::Block(inner) => stores(function, inner, variable),
            Statement::If { accept, reject, .. } => {
                stores(function, accept, variable) + stores(function, reject, variable)
            }
            Statement::Switch { cases, .. } => cases
                .iter()
                .map(|case| stores(function, &case.body, variable))
                .sum(),
            Statement::Loop {
                body, continuing, ..
            } => stores(function, body, variable) + stores(function, continuing, variable),
            _ => 0,
        })
        .sum()
}

/// Whether `variable`'s pointer reaches anything but a load or a store: an
/// access into it, a call's argument, an atomic or another expression.
fn pointer_escapes(function: &Function, variable: Handle<naga::LocalVariable>) -> bool {
    let names = |pointer: Handle<Expression>| local(function, pointer) == Some(variable);
    let in_expressions = function
        .expressions
        .iter()
        .any(|(_, expression)| match expression {
            Expression::Access { base, .. } | Expression::AccessIndex { base, .. } => names(*base),
            _ => false,
        });
    in_expressions || calls_with(&function.body, &names)
}

/// Whether a statement of `block` passes or operates on a pointer `names`
/// holds for, other than a load or a store.
fn calls_with(block: &Block, names: &dyn Fn(Handle<Expression>) -> bool) -> bool {
    block.iter().any(|statement| match statement {
        Statement::Call { arguments, .. } => arguments.iter().any(|&argument| names(argument)),
        Statement::Atomic { pointer, .. } => names(*pointer),
        Statement::Block(inner) => calls_with(inner, names),
        Statement::If { accept, reject, .. } => {
            calls_with(accept, names) || calls_with(reject, names)
        }
        Statement::Switch { cases, .. } => cases.iter().any(|case| calls_with(&case.body, names)),
        Statement::Loop {
            body, continuing, ..
        } => calls_with(body, names) || calls_with(continuing, names),
        _ => false,
    })
}
