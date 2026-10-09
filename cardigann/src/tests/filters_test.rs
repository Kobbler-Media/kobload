use crate::definition::{FilterArgs, FilterDef, YamlString};
use crate::filters::apply_filters;

#[test]
fn test_filter_chain() {
    let filters = vec![
        FilterDef {
            name: "trim".to_string(),
            args: FilterArgs::None,
        },
        FilterDef {
            name: "prepend".to_string(),
            args: FilterArgs::Single(YamlString("https://example.com".to_string())),
        },
    ];
    assert_eq!(
        apply_filters("  /path  ", &filters),
        "https://example.com/path"
    );
}

#[test]
fn test_replace_filter() {
    let filters = vec![FilterDef {
        name: "replace".to_string(),
        args: FilterArgs::List(vec![
            YamlString(" ".to_string()),
            YamlString("+".to_string()),
        ]),
    }];
    assert_eq!(apply_filters("hello world", &filters), "hello+world");
}
