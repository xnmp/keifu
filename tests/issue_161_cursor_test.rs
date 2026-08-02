use keifu::{text_editor::TextEditor, ui::issue_compose::editor_layout};

#[test]
fn new_issue_cursor_advances_past_inserted_spaces() {
    let mut title = TextEditor::from_text("Issue");

    title.insert_char(' ');
    assert_eq!(editor_layout(&title, 20, 3).cursor, Some((6, 0)));
    title.insert_char(' ');
    assert_eq!(editor_layout(&title, 20, 3).cursor, Some((7, 0)));

    title.insert_char('t');
    assert_eq!(title.text, "Issue  t");
    assert_eq!(editor_layout(&title, 20, 3).cursor, Some((8, 0)));
}
