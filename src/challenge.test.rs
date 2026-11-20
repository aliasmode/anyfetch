use super::*;

#[test]
fn knows_a_waiting_page_by_its_title() {
    assert!(waiting_page(
        "<html><head><title>Pardon Our Interruption...</title></head></html>"
    ));
    assert!(waiting_page("<head>\n<title> Client Challenge </title>"));
    assert!(!waiting_page("<title>A client challenge in court</title>"));
    assert!(!waiting_page("<html><body>No title</body></html>"));
}

#[test]
fn finds_the_cookie_of_a_reload_check_in_its_known_form_only() {
    let page = |value: &str| {
        format!(
            "<html><body><script>\n document.cookie = \"js_challenge_value={value}; Path=/\";\n \
             window.location.reload();\n</script></body></html>"
        )
    };
    assert_eq!(
        cookie(&page("feeef5c1")).as_deref(),
        Some("js_challenge_value=feeef5c1")
    );
    assert!(cookie(&page("not hex")).is_none());
    assert!(cookie("<script>document.cookie = \"theme=dark; Path=/\";</script>").is_none());
    assert!(
        cookie("<script>document.cookie = \"js_challenge_value=ab; Path=/\";</script>")
            .is_none()
    );
}

#[test]
fn answers_the_doubled_string_check_with_the_form_token() {
    let page = "<html><body><script>x=(await(async e=>e+e)(\"884c3f4d\"));</script>\
        <form hidden method=\"GET\" action=\"/r/rust/\"><input type=\"hidden\" name=\"solution\" />\
        <input type=\"hidden\" name=\"js_challenge\" value=\"1\"/>\
        <input type=\"hidden\" name=\"jsc_token\" value=\"2824be10\"/>\
        <input type=\"hidden\" name=\"jsc_orig_r\" value=\"\"/></form></body></html>";
    assert_eq!(
        doubled(page).as_deref(),
        Some("solution=884c3f4d884c3f4d&js_challenge=1&jsc_token=2824be10&jsc_orig_r=")
    );
    // Another function is another check.
    assert!(doubled(&page.replace("e=>e+e", "e=>e+e+e")).is_none());
    assert!(doubled("<html><body>Reddit</body></html>").is_none());
}
