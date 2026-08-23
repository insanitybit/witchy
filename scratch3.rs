    for func in &mut module.funcs {
        if let Some(target) = clones.get(&func.name) {
            let mut changed = false;
            let mut new_locals = Vec::new();
            inline_seq(&mut func.body, target, &func.name, &func.params, &func.locals, &mut new_locals, &mut changed);
            if changed {
                func.locals.extend(new_locals);
            }
        }
    }
