fn main() {
    // gtk4-layer-shell stands in for some of libwayland's functions, which only works when
    // it comes before libwayland among the libraries loaded; the glass of the desktop's
    // menus links libwayland in through the library, which would put it first otherwise.
    println!("cargo:rustc-link-lib=dylib=gtk4-layer-shell");
}
