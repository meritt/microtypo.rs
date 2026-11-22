use microtypo_conformance::HOST;
use microtypo_core::MicroTypo;
use serde_json::json;

#[test]
fn client_html_cases() {
    let mut engine = MicroTypo::new(Some(&json!({"input": "html", "html": false})), HOST).unwrap();
    let cases = [
        ("<p>Он в доме и саду.</p>", "<p>Он в доме и саду.</p>"),
        (
            r#"<p>Он в <em>доме</em> и <a href="/x">саду</a>.</p>"#,
            r#"<p>Он в <em>доме</em> и <a href="/x">саду</a>.</p>"#,
        ),
        (
            r#"<p>"<em>слово</em> и <a href="/x">другое</a>"</p>"#,
            r#"<p>«<em>слово</em> и <a href="/x">другое</a>»</p>"#,
        ),
        (
            "<p>\"<em>слово</em> и <a href=\"/x\">другое</a>\"</p>\n<p><code>\"x\" -- a  b</code></p>",
            "<p>«<em>слово</em> и <a href=\"/x\">другое</a>»</p>\n<p><code>\"x\" -- a  b</code></p>",
        ),
        (
            r#"<figure><img src="/x"><figcaption>"Подпись"</figcaption></figure>"#,
            r#"<figure><img src="/x"><figcaption>«Подпись»</figcaption></figure>"#,
        ),
        (
            "<p>\"<em>слово</em> и <a href=\"/x\">другое</a>\"</p>\n<figure><img src=\"/x\"><figcaption>\"Подпись\"</figcaption></figure>",
            "<p>«<em>слово</em> и <a href=\"/x\">другое</a>»</p>\n<figure><img src=\"/x\"><figcaption>«Подпись»</figcaption></figure>",
        ),
        (
            "<p>\"<em>слово</em> и <a href=\"/x\">другое</a>\"</p>\n",
            "<p>«<em>слово</em> и <a href=\"/x\">другое</a>»</p>",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(engine.process(input).unwrap(), expected, "{input}");
    }
}
