use chrono::{Local, TimeZone};
use git2::Oid;
use keifu::{
    git::{
        graph::{build_graph, CellType, GraphLayout, SquashMergeLine},
        CommitInfo,
    },
    graph::colors::SQUASH_LINK_COLOR_INDEX,
};

fn oid(byte: u8) -> Oid {
    Oid::from_bytes(&[byte; 20]).unwrap()
}

fn commit(oid: Oid, parent_oids: Vec<Oid>) -> CommitInfo {
    CommitInfo {
        oid,
        short_id: oid.to_string()[..7].to_string(),
        author_name: "Test Author".into(),
        author_email: "test@example.com".into(),
        timestamp: Local.timestamp_opt(0, 0).single().unwrap(),
        message: "test".into(),
        full_message: "test".into(),
        parent_oids,
    }
}

fn row_of(layout: &GraphLayout, oid: Oid) -> usize {
    layout
        .nodes
        .iter()
        .position(|node| node.commit.as_ref().map(|commit| commit.oid) == Some(oid))
        .expect("commit is present in the graph layout")
}

fn cell_color(cell: CellType) -> Option<usize> {
    match cell {
        CellType::Empty => None,
        CellType::Pipe(color)
        | CellType::Commit(color)
        | CellType::BranchRight(color)
        | CellType::BranchLeft(color)
        | CellType::MergeRight(color)
        | CellType::MergeLeft(color)
        | CellType::Horizontal(color)
        | CellType::TeeRight(color)
        | CellType::TeeLeft(color)
        | CellType::TeeUp(color)
        | CellType::HorizontalPipe(color, _) => Some(color),
        CellType::TeeDown(_, stem_color) => Some(stem_color),
    }
}

#[test]
fn squash_link_duplicate_relationship_is_idempotent() {
    let (squash_commit, branch_tip, trunk_parent, shared_base) = (oid(1), oid(4), oid(2), oid(9));
    let commits = vec![
        commit(squash_commit, vec![trunk_parent]),
        commit(branch_tip, vec![shared_base]),
        commit(trunk_parent, vec![shared_base]),
        commit(shared_base, vec![]),
    ];
    let single_line = SquashMergeLine::new(branch_tip, squash_commit);
    let single = build_graph(&commits, &[], &[], &[], None, None, &[single_line]);
    let duplicate = build_graph(
        &commits,
        &[],
        &[],
        &[],
        None,
        None,
        &[single_line, single_line],
    );

    let squash_row = row_of(&duplicate, squash_commit);
    let tip_row = row_of(&duplicate, branch_tip);
    let connector_col = duplicate.nodes[tip_row].lane * 2;
    let grey_cells: Vec<_> = duplicate
        .nodes
        .iter()
        .enumerate()
        .flat_map(|(row, node)| {
            node.cells
                .iter()
                .enumerate()
                .filter_map(move |(col, cell)| {
                    (cell_color(*cell) == Some(SQUASH_LINK_COLOR_INDEX)).then_some((
                        row,
                        col,
                        *cell,
                        node.cell_oids[col],
                    ))
                })
        })
        .collect();

    assert_eq!(squash_row + 1, tip_row);
    assert_eq!(
        duplicate.nodes[squash_row].commit.as_ref().unwrap().oid,
        squash_commit
    );
    assert_eq!(
        duplicate.nodes[tip_row].commit.as_ref().unwrap().oid,
        branch_tip
    );
    assert_eq!(
        grey_cells,
        vec![
            (
                squash_row,
                connector_col - 1,
                CellType::Horizontal(SQUASH_LINK_COLOR_INDEX),
                (None, None),
            ),
            (
                squash_row,
                connector_col,
                CellType::BranchLeft(SQUASH_LINK_COLOR_INDEX),
                (None, None),
            ),
        ],
        "duplicate aliases produce one exact connector from squash landing to branch-tip dot",
    );
    assert!(matches!(
        duplicate.nodes[tip_row].cells[connector_col],
        CellType::Commit(_)
    ));
    assert_eq!(duplicate.max_lane, single.max_lane);
    assert_eq!(duplicate.nodes.len(), single.nodes.len());
    for (actual, expected) in duplicate.nodes.iter().zip(&single.nodes) {
        assert_eq!(actual.cells, expected.cells);
        assert_eq!(actual.cell_oids, expected.cell_oids);
    }
}
