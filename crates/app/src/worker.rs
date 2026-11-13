//! Background layout thread. The UI thread never computes a layout or holds
//! the tree lock for longer than a hover lookup.

use spacescope_core::{layout, Filter, Layout, LayoutOptions, NodeId, Shared};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

pub struct Request {
    pub serial: u64,
    pub shared: Arc<Shared>,
    pub root: NodeId,
    pub opts: LayoutOptions,
    /// `(id, filter)`; the id changes whenever the filter text does.
    pub filter: Option<(u64, Arc<Filter>)>,
}

pub struct Reply {
    pub serial: u64,
    pub layout: Layout,
}

pub struct LayoutWorker {
    tx: Sender<Request>,
    pub rx: Receiver<Reply>,
}

impl LayoutWorker {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, req_rx) = channel::<Request>();
        let (reply_tx, rx) = channel::<Reply>();
        std::thread::Builder::new()
            .name("layout".into())
            .spawn(move || {
                let mut weights: Vec<u64> = Vec::new();
                let mut cached: Option<(usize, u64, u64, bool)> = None;
                while let Ok(mut req) = req_rx.recv() {
                    // Only the newest request matters.
                    while let Ok(newer) = req_rx.try_recv() {
                        req = newer;
                    }
                    let generation = req.shared.generation();
                    let tree = req.shared.tree.read();
                    let w = match &req.filter {
                        Some((id, filter)) => {
                            let key = (
                                Arc::as_ptr(&req.shared) as usize,
                                *id,
                                generation,
                                req.opts.metric == spacescope_core::Metric::Logical,
                            );
                            if cached != Some(key) {
                                filter.compute_weights(&tree, req.opts.metric, &mut weights);
                                cached = Some(key);
                            }
                            Some(&weights[..])
                        }
                        None => None,
                    };
                    let layout = layout::compute(&tree, w, req.root, &req.opts);
                    drop(tree);
                    if reply_tx
                        .send(Reply {
                            serial: req.serial,
                            layout,
                        })
                        .is_err()
                    {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("failed to start layout thread");
        LayoutWorker { tx, rx }
    }

    pub fn send(&self, req: Request) {
        let _ = self.tx.send(req);
    }
}
