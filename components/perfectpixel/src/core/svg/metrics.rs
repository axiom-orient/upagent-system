use super::ir::{PathCommand, SvgIr};

pub(super) struct Metrics {
    pub path_count: usize,
    pub node_count: usize,
    pub curve_segment_count: usize,
    pub closed_path_count: usize,
}

pub(super) fn collect(ir: &SvgIr) -> Metrics {
    let mut metrics = Metrics {
        path_count: 0,
        node_count: 0,
        curve_segment_count: 0,
        closed_path_count: 0,
    };
    for element in &ir.elements {
        if element.local_name != "path" {
            continue;
        }
        metrics.path_count += 1;
        if let Some(path) = &element.path {
            let mut closed = false;
            for command in &path.commands {
                match command {
                    PathCommand::Move => {}
                    PathCommand::Line => metrics.node_count += 1,
                    PathCommand::Curve => {
                        metrics.node_count += 1;
                        metrics.curve_segment_count += 1;
                    }
                    PathCommand::Close => closed = true,
                }
            }
            if closed {
                metrics.closed_path_count += 1;
            }
        }
    }
    metrics
}
