fn sample(x: bool) -> Option<bool> {
    let nested = || if x { 1 } else { 0 };
    let value = Some(true)?;
    if value && x {
        return None;
    }
    match nested() {
        0 => Some(false),
        n if n > 1 => Some(true),
        _ => Some(value),
    }
}
