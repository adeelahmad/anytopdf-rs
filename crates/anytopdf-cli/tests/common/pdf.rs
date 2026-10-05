pub fn page_count(pdf: &[u8]) -> u64 {
    let text = String::from_utf8_lossy(pdf);
    regex::Regex::new(r"/Type\s*/Page\b")
        .unwrap()
        .find_iter(&text)
        .count() as u64
}
