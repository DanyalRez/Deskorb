//! Intègre les deux logos dans l'exe (1 = Orbite, montré par l'Explorateur ; 2 = Jauges).

fn main() {
    println!("cargo:rerun-if-changed=assets/deskorb.rc");
    println!("cargo:rerun-if-changed=assets/deskorb-orbite.ico");
    println!("cargo:rerun-if-changed=assets/deskorb-jauges.ico");
    let obj = format!("{}/deskorb-res.o", std::env::var("OUT_DIR").unwrap());
    // shortcut: windres vient de la chaîne MinGW (cible GNU) ; avec MSVC il faudrait rc.exe
    let ok = std::process::Command::new("windres")
        .args(["assets/deskorb.rc", "-O", "coff", "-o", &obj])
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        println!("cargo:rustc-link-arg-bins={obj}");
    } else {
        println!("cargo:warning=windres introuvable : l'exe n'aura pas d'icône");
    }
}
