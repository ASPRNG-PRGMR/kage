//! GLSL sources, assembled from the files in `shaders/`.
//!
//! The rendering code lives once, in `common.glsl`, and the parameter block
//! once, in `params.glsl`. A stage file pulls them in with a line of the
//! form `//#include <file>`; that is resolved here, at start-up, by plain
//! text substitution (GLSL itself has no include directive).

const PARAMS: &str = include_str!("../../shaders/params.glsl");
const COMMON: &str = include_str!("../../shaders/common.glsl");
const VERTEX: &str = include_str!("../../shaders/render.vert");
const FRAGMENT: &str = include_str!("../../shaders/render.frag");
const COMPUTE: &str = include_str!("../../shaders/render.comp");

fn assemble(stage: &str) -> String {
    let mut out = String::with_capacity(stage.len() + PARAMS.len() + COMMON.len());
    for line in stage.lines() {
        match line.trim() {
            "//#include params.glsl" => out.push_str(PARAMS),
            "//#include common.glsl" => out.push_str(COMMON),
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// Vertex shader of the OpenGL backend: a full-target triangle.
pub fn vertex() -> String {
    VERTEX.to_owned()
}

/// Fragment shader of the OpenGL backend (GLSL 3.30).
pub fn fragment() -> String {
    assemble(FRAGMENT)
}

/// Compute shader of the Vulkan backend (GLSL 4.50).
pub fn compute() -> String {
    assemble(COMPUTE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_are_resolved() {
        for src in [fragment(), compute()] {
            assert!(!src.contains("//#include"), "unresolved include");
            assert!(src.contains("ivec4 dims;"), "params block missing");
            assert!(src.contains("vec4 kage_render(ivec2 p)"), "common code missing");
            assert!(src.starts_with("#version"), "version must be the first line");
        }
    }

    #[test]
    fn params_block_lists_the_fields_of_shader_params_in_order() {
        let fields: Vec<&str> = PARAMS
            .lines()
            .filter(|l| !l.trim_start().starts_with("//") && l.contains(';'))
            .map(|l| l.split(';').next().unwrap().split_whitespace().last().unwrap())
            .collect();
        assert_eq!(fields, ["dims", "cell", "taps_h[3]", "taps_v[3]", "footprint", "edge", "sites[12]"]);
    }
}
