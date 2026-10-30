use std::thread;

/// Item count below which the helpers run inline instead of spawning workers.
///
/// Spawning a thread costs more than a handful of cheap per-item operations, and several
/// call sites routinely pass very small sets (a transaction's removals, a single atlas page,
/// a two-state normalize request). This is a count-based heuristic because the helpers cannot
/// know per-item cost. Small inputs avoid worker startup overhead.
const MIN_PARALLEL_ITEMS: usize = 4;

fn worker_count(items: usize) -> usize {
    thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .min(items)
        .max(1)
}

/// Runs `work` over `items` on a bounded pool of scoped threads and returns results in the
/// original order. Each worker owns a disjoint contiguous slice of the output, so results
/// never race. If more than one item fails, the returned error is always the one at the
/// lowest original index, regardless of which worker finishes first — callers that depend on
/// deterministic failure reporting must not observe a different error across runs.
///
/// A panicking `work` propagates the original panic, exactly as the sequential loop it
/// replaces would.
pub fn parallel_map<T, R, E>(
    items: &[T],
    work: impl Fn(&T) -> Result<R, E> + Sync,
) -> Result<Vec<R>, E>
where
    T: Sync,
    R: Send,
    E: Send,
{
    if items.len() < MIN_PARALLEL_ITEMS {
        return items.iter().map(work).collect();
    }
    let mut slots: Vec<Option<Result<R, E>>> = (0..items.len()).map(|_| None).collect();
    let chunk_size = items.len().div_ceil(worker_count(items.len()));

    // `thread::scope` joins every worker it spawned and resumes any worker panic here, so a
    // panicking `work` never lets execution reach the partially filled slots below.
    thread::scope(|scope| {
        for (item_chunk, slot_chunk) in items.chunks(chunk_size).zip(slots.chunks_mut(chunk_size)) {
            let work = &work;
            scope.spawn(move || {
                for (item, slot) in item_chunk.iter().zip(slot_chunk) {
                    *slot = Some(work(item));
                }
            });
        }
    });

    // `Result`'s `FromIterator` short-circuits on the first `Err` in iteration order, which is
    // original index order — that is the lowest-index-error-wins guarantee, by construction.
    slots
        .into_iter()
        .map(|slot| slot.expect("every slot is populated once its worker joined"))
        .collect()
}

/// Owned-item counterpart of [`parallel_map`] for work that must consume each item (no
/// borrow-friendly signature available). Chunks stay contiguous in original order, so
/// flattening chunk results in order reproduces both the original index order and the
/// lowest-index-error-wins guarantee without needing shared mutable slots.
pub fn parallel_map_owned<T, R, E>(
    items: Vec<T>,
    work: impl Fn(T) -> Result<R, E> + Sync,
) -> Result<Vec<R>, E>
where
    T: Send,
    R: Send,
    E: Send,
{
    if items.len() < MIN_PARALLEL_ITEMS {
        return items.into_iter().map(work).collect();
    }
    let chunk_size = items.len().div_ceil(worker_count(items.len()));
    let mut chunks = Vec::new();
    let mut rest = items;
    while !rest.is_empty() {
        let tail = rest.split_off(chunk_size.min(rest.len()));
        chunks.push(rest);
        rest = tail;
    }

    let chunk_results = thread::scope(|scope| {
        let handles = chunks
            .into_iter()
            .map(|chunk| {
                let work = &work;
                scope.spawn(move || chunk.into_iter().map(work).collect::<Vec<_>>())
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(chunk) => chunk,
                // Joining explicitly opts out of `thread::scope`'s automatic panic
                // propagation, so re-raise the original payload rather than replacing it
                // with a new panic — `parallel_map` reports worker panics the same way.
                Err(payload) => std::panic::resume_unwind(payload),
            })
            .collect::<Vec<_>>()
    });

    chunk_results.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Comfortably above `MIN_PARALLEL_ITEMS` so the tests exercise the threaded path.
    const PARALLEL: usize = MIN_PARALLEL_ITEMS * 2;

    #[test]
    fn preserves_original_order() {
        let items = (0..PARALLEL).rev().collect::<Vec<_>>();
        let result = parallel_map(&items, |value| Ok::<_, ()>(value * 10)).unwrap();
        let expected = (0..PARALLEL)
            .rev()
            .map(|value| value * 10)
            .collect::<Vec<_>>();
        assert_eq!(result, expected);
    }

    #[test]
    fn empty_input_returns_empty_output() {
        let items: Vec<u32> = Vec::new();
        let result = parallel_map(&items, |value| Ok::<_, ()>(*value)).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn lowest_index_error_wins_deterministically() {
        let items = (0..PARALLEL).collect::<Vec<usize>>();
        for _ in 0..20 {
            let error = parallel_map(&items, |value| {
                if value % 2 == 0 {
                    Err(*value)
                } else {
                    Ok(*value)
                }
            })
            .unwrap_err();
            assert_eq!(error, 0);
        }
    }

    #[test]
    fn small_input_runs_inline_with_identical_results() {
        for len in 0..MIN_PARALLEL_ITEMS {
            let items = (0..len).collect::<Vec<usize>>();
            let result = parallel_map(&items, |value| Ok::<_, ()>(value * 3)).unwrap();
            assert_eq!(
                result,
                items.iter().map(|value| value * 3).collect::<Vec<_>>()
            );

            let owned = parallel_map_owned(items.clone(), |value| Ok::<_, ()>(value * 3)).unwrap();
            assert_eq!(owned, result);
        }
    }

    #[test]
    fn small_input_still_reports_the_lowest_index_error() {
        let items = vec![0usize, 1, 2];
        let error = parallel_map(
            &items,
            |value| {
                if *value == 0 {
                    Err(*value)
                } else {
                    Ok(*value)
                }
            },
        )
        .unwrap_err();
        assert_eq!(error, 0);
    }

    #[test]
    fn owned_preserves_original_order() {
        let items = (0..PARALLEL).rev().collect::<Vec<_>>();
        let expected = items.iter().map(|value| value * 10).collect::<Vec<_>>();
        let result = parallel_map_owned(items, |value| Ok::<_, ()>(value * 10)).unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn owned_lowest_index_error_wins_deterministically() {
        let items = (0..PARALLEL).collect::<Vec<usize>>();
        for _ in 0..20 {
            let error = parallel_map_owned(items.clone(), |value| {
                if value % 2 == 0 {
                    Err(value)
                } else {
                    Ok(value)
                }
            })
            .unwrap_err();
            assert_eq!(error, 0);
        }
    }

    #[test]
    fn worker_panic_propagates_from_both_helpers() {
        let items = (0..PARALLEL).collect::<Vec<usize>>();

        let borrowed = std::panic::catch_unwind(|| {
            let _ = parallel_map(&items, |value| -> Result<usize, ()> {
                assert_ne!(*value, PARALLEL - 1, "borrowed worker panic marker");
                Ok(*value)
            });
        });
        assert!(
            borrowed.is_err(),
            "parallel_map must propagate worker panics"
        );

        let owned_items = items.clone();
        let owned = std::panic::catch_unwind(move || {
            let _ = parallel_map_owned(owned_items, |value| -> Result<usize, ()> {
                assert_ne!(value, PARALLEL - 1, "owned worker panic marker");
                Ok(value)
            });
        });
        assert!(
            owned.is_err(),
            "parallel_map_owned must propagate worker panics"
        );
    }
}
