use super::*;

#[test]
fn reads_the_cookies_of_one_site_in_both_expiry_units() {
    for (expiry, expected) in [
        (1_900_000_000_i64, 1_900_000_000),
        (1_900_000_000_123, 1_900_000_000),
    ] {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "anyfetch-cookie-test-{}-{expiry}",
            std::process::id()
        )));
        let profile = scratch.0.join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let database = rusqlite::Connection::open(profile.join("cookies.sqlite")).unwrap();
        database
            .execute_batch(&format!(
                "CREATE TABLE moz_cookies (name TEXT, value TEXT, host TEXT, expiry INTEGER);
                 INSERT INTO moz_cookies VALUES ('cf_clearance', 'pass', '.example.org', {expiry});
                 INSERT INTO moz_cookies VALUES ('session', 's1', 'www.example.org', 0);
                 INSERT INTO moz_cookies VALUES ('cf_clearance', 'other', '.example.com', {expiry});"
            ))
            .unwrap();
        drop(database);
        let (cookies, expires) = read_cookies(&scratch.0, "www.example.org");
        assert_eq!(expires, expected);
        let names: Vec<_> = cookies
            .iter()
            .map(|cookie| (&*cookie.name, &*cookie.value))
            .collect();
        assert_eq!(names, [("cf_clearance", "pass"), ("session", "s1")]);
        assert_eq!(read_cookies(&scratch.0, "example.net").1, 0);
    }
}
