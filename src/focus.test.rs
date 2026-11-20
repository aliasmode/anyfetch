use super::*;

fn page() -> String {
    let filler = |topic: &str| {
        (1..=12)
            .map(|number| format!("Sentence {number} about {topic} says nothing of note."))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    [
        "# Client guide",
        &filler("installation"),
        "## Retries",
        &filler("retries"),
        "## Timeouts",
        "The default is 30 seconds. Set another value like this:",
        "```toml\n[client]\n\nwait = 5\n```",
        &filler("limits"),
        "## Proxies",
        &filler("proxies"),
    ]
    .join("\n\n")
}

#[test]
fn returns_the_matching_parts_in_page_order_under_their_headings() {
    let page = page();
    let focused = focus(&page, "What is the default timeout?", 400).unwrap();
    assert!(
        focused.starts_with(
            "[...]\n\n# Client guide\n\n## Timeouts\n\nThe default is 30 seconds. \
             Set another value like this:\n\n```toml\n[client]\n\nwait = 5\n```\n\n"
        ),
        "{focused}"
    );
    assert!(focused.ends_with("[...]"));
    let raw = page.replace("## Timeouts\n\n", "## Timeouts\n");
    let focused = focus(&raw, "What is the default timeout?", 400).unwrap();
    assert!(focused.contains("## Timeouts\n\nThe default is 30 seconds."));
    assert!(!focused.contains("## Retries") && !focused.contains("## Proxies"));
    for budget in [60, 150, 400, 900, 2_000] {
        let focused = focus(&page, "default timeout sentence proxies", budget);
        let length = focused.map_or(0, |focused| focused.chars().count());
        assert!(length <= budget, "{length} > {budget}");
    }
}

#[test]
fn keeps_the_whole_page_when_it_fits_or_nothing_matches() {
    let page = page();
    assert!(focus(&page, "default timeout", page.len()).is_none());
    assert!(focus(&page, "kubernetes", 400).is_none());
    assert!(focus(&page, "", 400).is_none());
    let link = "[Sentence](https://example.org/kubernetes \"Kubernetes\") 1 ";
    let linked = page.replace("Sentence 1 ", link);
    assert!(linked.contains("kubernetes") && focus(&linked, "kubernetes", 400).is_none());
}

#[test]
fn a_part_of_a_long_table_or_document_keeps_its_context() {
    let rows = (1..=200)
        .map(|number| format!("| item{number} | {} |", number * 7))
        .collect::<Vec<_>>();
    let table = format!(
        "Prices:\n\n| Name | Price |\n| --- | --- |\n{}",
        rows.join("\n")
    );
    let focused = focus(&table, "item150", 300).unwrap();
    assert!(
        focused.starts_with("[...]\n\n| Name | Price |\n| --- | --- |\n| item1"),
        "{focused}"
    );
    assert!(focused.contains("| item150 | 1050 |"));

    let lines = |page: usize| {
        (1..=40)
            .map(|line| format!("Page {page} line {line} of the report."))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let pdf = format!(
        "[Page 1]\n\n{}\n\n[Page 2]\n\n{}\nThe melting point is 1538 degrees.\n{}",
        lines(1),
        lines(2),
        lines(2)
    );
    let focused = focus(&pdf, "melting point", 1_200).unwrap();
    assert!(focused.contains("[Page 2]") && !focused.contains("[Page 1]"));
    assert!(focused.contains("The melting point is 1538 degrees."));
}
