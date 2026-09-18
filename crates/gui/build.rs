fn main() {
    // Plain `fluent`, not `fluent-dark`: the suffix folds the platform's
    // color scheme to a constant, and the window writes the widget palette's
    // scheme itself from the one the user chose.
    println!("cargo:rustc-env=SLINT_STYLE=fluent");
    // Slint's element-query API, which the pointer-target tests use to click a
    // real button in a real window, can only find an element when the compiler
    // kept its debug info. Release builds do not need it and should not carry
    // it, so it is emitted for every other profile.
    if std::env::var("PROFILE").as_deref() != Ok("release") {
        println!("cargo:rustc-env=SLINT_EMIT_DEBUG_INFO=1");
    }
}
