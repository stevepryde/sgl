//! A game's derivatives in uniform control flow. WGSL lets a fragment take a
//! derivative (`dpdx`, `dpdy`, `fwidth` and their fine and coarse forms)
//! only where every invocation of its quad takes it, in uniform control flow
//! (the WGSL specification's "Uniformity" analysis), and Chrome's compiler
//! refuses a module that may take one elsewhere, where the naga `add_shader`
//! validates with accepts it. SGL3D calls `material_surface` in uniform
//! control flow, so a game's module is held to a conservative form of the
//! rule, which needs no analysis of which values are uniform: a function
//! takes a derivative, or calls a function that takes one, only in its
//! top-level statements, outside `if`, `switch` and loops and before any
//! `return` within them. The right of `&&` and `||`, which WGSL evaluates
//! only as the left's value has it, is within an `if` as naga lowers it
//! (naga 30.0.1 `front/wgsl/lower/mod.rs`, `logical`).
use crate::content::shader::ShaderError;
use naga::{Block, Expression, Function, Handle, Module, Statement};
use std::collections::HashMap;

/// Checks every function of `module` whose name `game` holds for.
pub(super) fn check(module: &Module, game: impl Fn(&str) -> bool) -> Result<(), ShaderError> {
    let mut derives = HashMap::new();
    for (_, function) in module.functions.iter() {
        let name = function.name.clone().unwrap_or_default();
        if game(&name) && !top_level(module, function, &mut derives) {
            return Err(ShaderError::NonUniformDerivative { function: name });
        }
    }
    Ok(())
}

/// Whether `function`'s derivatives, and its calls of functions that take
/// one, all lie in its top-level statements before any `return` within
/// control flow.
fn top_level(
    module: &Module,
    function: &Function,
    derives: &mut HashMap<Handle<Function>, bool>,
) -> bool {
    let mut returned = false;
    let mut statements = function.body.iter().collect::<Vec<_>>();
    // A `{}` block is no control flow: its statements are the function's.
    while let Some(index) = statements
        .iter()
        .position(|statement| matches!(statement, Statement::Block(_)))
    {
        let Statement::Block(inner) = statements[index] else {
            unreachable!()
        };
        statements.splice(index..=index, inner.iter());
    }
    for statement in statements {
        match statement {
            Statement::If { accept, reject, .. } => {
                if deriving(module, function, accept, derives)
                    || deriving(module, function, reject, derives)
                {
                    return false;
                }
                returned |= returns(accept) || returns(reject);
            }
            Statement::Switch { cases, .. } => {
                if cases
                    .iter()
                    .any(|case| deriving(module, function, &case.body, derives))
                {
                    return false;
                }
                returned |= cases.iter().any(|case| returns(&case.body));
            }
            Statement::Loop {
                body, continuing, ..
            } => {
                if deriving(module, function, body, derives)
                    || deriving(module, function, continuing, derives)
                {
                    return false;
                }
                returned |= returns(body) || returns(continuing);
            }
            statement => {
                if returned && derives_at(module, function, statement, derives) {
                    return false;
                }
            }
        }
    }
    true
}

/// Whether `block`, at any depth, takes a derivative or calls a function
/// that takes one.
fn deriving(
    module: &Module,
    function: &Function,
    block: &Block,
    derives: &mut HashMap<Handle<Function>, bool>,
) -> bool {
    block.iter().any(|statement| match statement {
        Statement::Block(inner) => deriving(module, function, inner, derives),
        Statement::If { accept, reject, .. } => {
            deriving(module, function, accept, derives)
                || deriving(module, function, reject, derives)
        }
        Statement::Switch { cases, .. } => cases
            .iter()
            .any(|case| deriving(module, function, &case.body, derives)),
        Statement::Loop {
            body, continuing, ..
        } => {
            deriving(module, function, body, derives)
                || deriving(module, function, continuing, derives)
        }
        statement => derives_at(module, function, statement, derives),
    })
}

/// Whether `statement` itself takes a derivative (an expression it emits)
/// or calls a function that takes one.
fn derives_at(
    module: &Module,
    function: &Function,
    statement: &Statement,
    derives: &mut HashMap<Handle<Function>, bool>,
) -> bool {
    match statement {
        Statement::Emit(range) => range.clone().any(|expression| {
            matches!(
                function.expressions[expression],
                Expression::Derivative { .. }
            )
        }),
        Statement::Call {
            function: callee, ..
        } => derives_in(module, *callee, derives),
        _ => false,
    }
}

/// Whether a call of `handle` takes a derivative, in it or in a function it
/// calls (WGSL has no recursion).
fn derives_in(
    module: &Module,
    handle: Handle<Function>,
    derives: &mut HashMap<Handle<Function>, bool>,
) -> bool {
    if let Some(&known) = derives.get(&handle) {
        return known;
    }
    let function = &module.functions[handle];
    let found = deriving(module, function, &function.body, derives);
    derives.insert(handle, found);
    found
}

/// Whether `block` returns somewhere within it.
fn returns(block: &Block) -> bool {
    block.iter().any(|statement| match statement {
        Statement::Return { .. } => true,
        Statement::Block(inner) => returns(inner),
        Statement::If { accept, reject, .. } => returns(accept) || returns(reject),
        Statement::Switch { cases, .. } => cases.iter().any(|case| returns(&case.body)),
        Statement::Loop {
            body, continuing, ..
        } => returns(body) || returns(continuing),
        _ => false,
    })
}
