//! Writes a small demo place with every basic shape: `cargo run --example demo_place -- demo.rbxl`.

use kahua_io::rbx_dom_weak::InstanceBuilder;
use kahua_io::rbx_dom_weak::types::{Color3, Enum, Vector3};
use kahua_io::template;
use kahua_render::glam::{EulerRot, Quat, Vec3};
use kahua_render::scene::cframe_from_pos_rot;

fn main() -> anyhow::Result<()> {
    let out = std::env::args().nth(1).unwrap_or_else(|| "demo.rbxl".into());
    let mut dom = template::baseplate();
    let ws = template::workspace(&dom);

    let part = |name: &str, class: &str, pos: Vec3, size: Vec3, rot_y: f32, color: (f32, f32, f32)| {
        InstanceBuilder::new(class)
            .with_name(name)
            .with_property("Anchored", true)
            .with_property("Size", Vector3::new(size.x, size.y, size.z))
            .with_property(
                "CFrame",
                cframe_from_pos_rot(pos, Quat::from_euler(EulerRot::YXZ, rot_y.to_radians(), 0.0, 0.0)),
            )
            .with_property("Color", Color3::new(color.0, color.1, color.2))
    };

    // A little house.
    let house = dom.insert(ws, InstanceBuilder::new("Model").with_name("House"));
    let walls = (0.85, 0.78, 0.62);
    dom.insert(
        house,
        part("Floor", "Part", Vec3::new(-14.0, 0.5, -6.0), Vec3::new(16.0, 1.0, 12.0), 0.0, (0.45, 0.33, 0.24)),
    );
    dom.insert(house, part("BackWall", "Part", Vec3::new(-14.0, 5.0, -11.5), Vec3::new(16.0, 8.0, 1.0), 0.0, walls));
    dom.insert(house, part("LeftWall", "Part", Vec3::new(-21.5, 5.0, -6.0), Vec3::new(1.0, 8.0, 10.0), 0.0, walls));
    dom.insert(house, part("RightWall", "Part", Vec3::new(-6.5, 5.0, -6.0), Vec3::new(1.0, 8.0, 10.0), 0.0, walls));
    dom.insert(
        house,
        part(
            "RoofFront",
            "WedgePart",
            Vec3::new(-14.0, 11.0, -3.0),
            Vec3::new(17.0, 4.0, 6.0),
            180.0,
            (0.62, 0.16, 0.14),
        ),
    );
    dom.insert(
        house,
        part("RoofBack", "WedgePart", Vec3::new(-14.0, 11.0, -9.0), Vec3::new(17.0, 4.0, 6.0), 0.0, (0.62, 0.16, 0.14)),
    );
    dom.insert(
        house,
        part("Window", "Part", Vec3::new(-14.0, 5.0, -0.6), Vec3::new(6.0, 4.0, 0.4), 0.0, (0.55, 0.8, 1.0))
            .with_property("Transparency", 0.5f32),
    );

    // Shapes showcase.
    let shapes = dom.insert(ws, InstanceBuilder::new("Folder").with_name("Shapes"));
    let shaped = |name: &str, shape: u32, x: f32, color| {
        part(name, "Part", Vec3::new(x, 3.0, 12.0), Vec3::new(5.0, 5.0, 5.0), 30.0, color)
            .with_property("Shape", Enum::from_u32(shape))
    };
    dom.insert(shapes, shaped("Ball", 0, 6.0, (0.95, 0.75, 0.1)));
    dom.insert(shapes, shaped("Block", 1, 13.0, (0.2, 0.55, 0.95)));
    dom.insert(
        shapes,
        part("Cylinder", "Part", Vec3::new(20.0, 3.0, 12.0), Vec3::new(6.0, 5.0, 5.0), 30.0, (0.3, 0.8, 0.35))
            .with_property("Shape", Enum::from_u32(2)),
    );
    dom.insert(
        shapes,
        part("Wedge", "WedgePart", Vec3::new(27.0, 3.0, 12.0), Vec3::new(5.0, 5.0, 5.0), 30.0, (0.9, 0.35, 0.6)),
    );
    dom.insert(
        shapes,
        part(
            "CornerWedge",
            "CornerWedgePart",
            Vec3::new(34.0, 3.0, 12.0),
            Vec3::new(5.0, 5.0, 5.0),
            30.0,
            (0.6, 0.4, 0.9),
        ),
    );

    // A staircase of spinning blocks.
    let stairs = dom.insert(ws, InstanceBuilder::new("Model").with_name("Stairs"));
    for i in 0..10 {
        let t = i as f32 / 9.0;
        dom.insert(
            stairs,
            part(
                &format!("Step{i}"),
                "Part",
                Vec3::new(14.0 + i as f32 * 2.2, 0.5 + i as f32 * 1.2, -10.0),
                Vec3::new(3.0, 1.0, 6.0),
                i as f32 * 6.0,
                (0.2 + 0.7 * t, 0.4, 0.9 - 0.6 * t),
            ),
        );
    }

    dom.insert(
        template::workspace(&dom),
        InstanceBuilder::new("Script").with_name("Hello").with_property("Source", "print(\"Hello from Kahua!\")\n"),
    );
    kahua_io::write_file(&dom, std::path::Path::new(&out))?;
    println!("wrote {out}");
    Ok(())
}
