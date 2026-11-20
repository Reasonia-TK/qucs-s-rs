//! qucs-s-rs – a lightweight Rust fork of the Qucs-S schematic editor with a
//! built-in MCP server, so AI agents see (and edit) exactly what the editor draws.

mod check;
mod doc;
mod gui;
mod library;
mod mcp;
mod netlist;
mod plot;
mod render;
mod router;
mod schematic;
mod sim;

use std::path::PathBuf;

fn usage() -> ! {
    eprintln!(
        "qucs-s-rs {}\n\
         usage:\n  \
         qucs-s-rs [gui] [file.sch]          open the editor\n  \
         qucs-s-rs mcp                       run the MCP server on stdio\n  \
         qucs-s-rs render <in.sch> <out.png|out.svg> [--overlay] [--scale S]\n  \
         qucs-s-rs check <in.sch>            layout report (JSON)\n  \
         qucs-s-rs reroute <in.sch> [out.sch] rip-up and re-route all nets",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("gui");
    match cmd {
        "mcp" => mcp::serve(),
        "render" => {
            if args.len() < 3 {
                usage();
            }
            let d = doc::Doc::open(&PathBuf::from(&args[1])).unwrap_or_else(|e| fail(&e));
            let conn = d.conn();
            let overlay = args.iter().any(|a| a == "--overlay");
            let scale = args.iter().position(|a| a == "--scale").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok());
            let rep = check::check(&d.sch, &conn);
            let ov = render::Overlay {
                color_nets: overlay,
                net_tags: overlay,
                pin_names: args.iter().any(|a| a == "--pins"),
                markers: if overlay { rep.issues.iter().filter(|i| i.severity == "error" || i.kind == "unconnected_pin").map(|i| (i.at.0, i.at.1, i.message.clone())).collect() } else { vec![] },
                ..Default::default()
            };
            let sc = render::build_scene(&d.sch, &conn, &ov);
            let view = render::fit_view(sc.bounds, 30.0, scale, 1600.0);
            let (svg, _) = render::scene_svg(&sc, &view, &render::Decor { grid: overlay, rulers: overlay, title: None });
            let out = &args[2];
            if out.ends_with(".svg") {
                std::fs::write(out, svg).unwrap_or_else(|e| fail(&e.to_string()));
            } else {
                let png = render::svg_to_png(&svg).unwrap_or_else(|e| fail(&e));
                std::fs::write(out, png).unwrap_or_else(|e| fail(&e.to_string()));
            }
            println!("{}", rep.metrics);
        }
        "check" => {
            if args.len() < 2 {
                usage();
            }
            let d = doc::Doc::open(&PathBuf::from(&args[1])).unwrap_or_else(|e| fail(&e));
            let rep = check::check(&d.sch, &d.conn());
            let v = serde_json::json!({"metrics": rep.metrics, "issues": rep.issues.iter().enumerate().map(|(k, i)| i.json(k + 1)).collect::<Vec<_>>()});
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
        }
        "reroute" => {
            if args.len() < 2 {
                usage();
            }
            let mut d = doc::Doc::open(&PathBuf::from(&args[1])).unwrap_or_else(|e| fail(&e));
            let r = d.reroute(&[]).unwrap_or_else(|e| fail(&e));
            let out = PathBuf::from(args.get(2).unwrap_or(&args[1]));
            d.sch.save(&out).unwrap_or_else(|e| fail(&e));
            println!("{r}");
        }
        "-h" | "--help" | "help" => usage(),
        "gui" => gui::run(args.get(1).map(PathBuf::from)),
        other if other.ends_with(".sch") => gui::run(Some(PathBuf::from(other))),
        _ => usage(),
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use crate::doc::Doc;
    use crate::schematic::Schematic;
    use std::path::Path;

    #[test]
    fn roundtrip_examples() {
        for f in ["examples/inverting_amp.sch", "examples/follower_rerouted.sch"] {
            let s = Schematic::load(Path::new(f)).unwrap();
            let again = Schematic::parse(&s.serialize()).unwrap();
            assert_eq!(s.components, again.components, "{f}");
            assert_eq!(s.wires, again.wires, "{f}");
        }
    }

    #[test]
    fn pins_follow_qucs_rotation() {
        // vertical resistor (rotate=1): pin 1 at the bottom, pin 2 at the top
        let c = crate::library::new_component("R", "R1", 100, 100).unwrap();
        let mut c1 = c.clone();
        c1.rotate = 1;
        let p = crate::library::place(&c1, None);
        assert_eq!((p.pins[0].x, p.pins[0].y), (100, 130));
        assert_eq!((p.pins[1].x, p.pins[1].y), (100, 70));
        assert_eq!(p.pins[0].dir, (0, 1));
        // DC source default orientation is vertical with + on top
        let v = crate::library::new_component("Vdc", "V1", 0, 0).unwrap();
        let p = crate::library::place(&v, None);
        let plus = p.pins.iter().find(|q| q.name == "+").unwrap();
        assert_eq!((plus.x, plus.y), (0, -30));
    }

    #[test]
    fn example_connectivity() {
        let d = Doc::open(Path::new("examples/inverting_amp.sch")).unwrap();
        let conn = d.conn();
        let net = |pin: &str| conn.nets[conn.find_pin(pin).unwrap().net].name.clone();
        assert_eq!(net("OP1.OUT"), "out");
        assert_eq!(net("R1.1"), "in");
        assert_eq!(net("V1.-"), "gnd");
        assert_eq!(net("R2.1"), net("OP1.IN-"));
        assert!(crate::check::check(&d.sch, &conn).issues.is_empty());
    }

    #[test]
    fn router_connects_cleanly() {
        let mut d = Doc::new(Schematic::new_empty("t"), None);
        let none = serde_json::Map::new();
        d.add_component("Vdc", Some("V1"), 0, 100, None, None, &none).unwrap();
        d.add_component("R", Some("R1"), 150, 40, None, None, &none).unwrap();
        d.add_component("R", Some("R2"), 260, 120, Some(1), None, &none).unwrap();
        d.add_component("GND", None, 0, 220, None, None, &none).unwrap();
        d.connect(&["V1.+".into(), "R1.1".into()], Some("in")).unwrap();
        d.connect(&["R1.2".into(), "R2.2".into()], Some("out")).unwrap();
        d.connect(&["V1.-".into(), "R2.1".into(), "GND1.1".into()], None).unwrap();
        let conn = d.conn();
        let net = |pin: &str| conn.nets[conn.find_pin(pin).unwrap().net].name.clone();
        assert_eq!(net("R1.1"), "in");
        assert_eq!(net("R2.2"), "out");
        assert_eq!(net("R2.1"), "gnd");
        let rep = crate::check::check(&d.sch, &conn);
        assert_eq!(rep.metrics["errors"], 0, "{:?}", rep.issues);
        assert_eq!(rep.metrics["crossings"], 0);
        assert!(d.sch.wires.iter().all(|w| w.is_horizontal() || w.is_vertical()));
    }
}
