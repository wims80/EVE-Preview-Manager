fn main() {
    for key in [
        "EPM_ESI_CLIENT_ID",
        "EPM_ESI_CALLBACK",
        "EPM_MAINTAINER_CONTACT",
        "EPM_ESI_COMPATIBILITY_DATE",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }

    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    if release {
        let missing: Vec<_> = [
            "EPM_ESI_CLIENT_ID",
            "EPM_ESI_CALLBACK",
            "EPM_MAINTAINER_CONTACT",
            "EPM_ESI_COMPATIBILITY_DATE",
        ]
        .into_iter()
        .filter(|key| std::env::var(key).map_or(true, |value| value.trim().is_empty()))
        .collect();
        assert!(
            missing.is_empty(),
            "ESI release compliance configuration is missing: {}",
            missing.join(", ")
        );
    }
}
