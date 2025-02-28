use poem_openapi::{types::Type, NewType};

#[tokio::test]
async fn new_type() {
    #[derive(NewType)]
    struct MyString(String);

    assert_eq!(MyString::schema_ref(), String::schema_ref());
}

#[tokio::test]
async fn new_type_summary_and_description() {
    /// MyString
    ///
    /// A
    /// B
    /// C
    #[derive(NewType)]
    struct MyString(String);

    let schema = MyString::schema_ref();
    let schema = schema.unwrap_inline();
    assert_eq!(schema.description, Some("MyString\n\nA\nB\nC"));
}
