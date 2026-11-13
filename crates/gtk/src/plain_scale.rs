//! Draw strongly reduced video frames without GTK's full-size copy.
//!
//! When a texture is drawn at less than half its size, GTK's GPU renderers sample it with mipmaps.
//! A video frame cannot carry mipmaps, so GTK first copies every such frame into a new full-size
//! image with mipmaps (about 44 MB for a 4K frame, more for 10-bit video). GTK's Vulkan renderer
//! does not check whether that allocation worked: when the driver refuses it, GTK hands the driver
//! a null memory handle and the app dies inside the driver (issue #17, seen with NVIDIA).
//!
//! [`PlainScaleBin`] rewrites what its child draws: a texture that would take that path becomes a
//! scaled-texture node with linear filtering, which GTK samples straight from the frame. Nothing
//! is copied; the price is a slightly rougher picture in small previews. Everything else the child
//! draws, and every texture GTK would not have copied, is passed through untouched.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};

/// GTK mipmaps above a reduction of 2. Staying a little below it covers rounding in GTK's own scale.
pub const REDUCTION: f32 = 1.9;

/// `true` when a `texture` (pixels) drawn into `bounds` (node units) at `scale` (pixels per node
/// unit) is reduced enough for GTK to want mipmaps.
pub fn needs_plain_scale(texture: (i32, i32), bounds: (f32, f32), scale: (f32, f32)) -> bool {
    texture.0 as f32 > REDUCTION * bounds.0 * scale.0 || texture.1 as f32 > REDUCTION * bounds.1 * scale.1
}

fn usable(scale: (f32, f32)) -> bool {
    scale.0.is_finite() && scale.1.is_finite() && scale.0 > 0.0 && scale.1 > 0.0
}

/// The scaled-texture node that replaces `node`, or `None` when GTK would not mipmap it.
fn plain_texture(node: &gsk::TextureNode, scale: (f32, f32)) -> Option<gsk::RenderNode> {
    let texture = node.texture();
    let bounds = node.bounds();
    if !needs_plain_scale((texture.width(), texture.height()), (bounds.width(), bounds.height()), scale) {
        return None;
    }
    // GTK draws a scaled-texture node at one pixel per node unit, so the node is laid out in
    // pixels and shrunk back by a transform. Without that it would be blurred on scaled screens.
    let pixels = graphene::Rect::new(bounds.x() * scale.0, bounds.y() * scale.1, bounds.width() * scale.0, bounds.height() * scale.1);
    let scaled = gsk::TextureScaleNode::new(&texture, &pixels, gsk::ScalingFilter::Linear);
    if scale == (1.0, 1.0) {
        return Some(scaled.upcast());
    }
    let shrink = gsk::Transform::new().scale(1.0 / scale.0, 1.0 / scale.1);
    Some(gsk::TransformNode::new(scaled, Some(&shrink)).upcast())
}

/// The scale below a transform node, or `None` when its child should be left alone.
fn scale_below(transform: &gsk::Transform, scale: (f32, f32)) -> Option<(f32, f32)> {
    use gsk::TransformCategory as Category;
    if !matches!(transform.category(), Category::_2d | Category::_2dAffine | Category::_2dTranslate | Category::Identity) {
        return None;
    }
    let (_skew_x, _skew_y, scale_x, scale_y, angle, _dx, _dy) = transform.to_2d_components();
    // A quarter turn swaps the axes; that only matters when the two scales differ.
    let turned = (angle.rem_euclid(180.0) - 90.0).abs() < 45.0;
    if turned && (scale.0 - scale.1).abs() > f32::EPSILON {
        return None;
    }
    let below = (scale.0 * scale_x.abs(), scale.1 * scale_y.abs());
    usable(below).then_some(below)
}

/// `node` with its strongly reduced textures replaced, or `None` when nothing in it changes.
fn rewritten(node: &gsk::RenderNode, scale: (f32, f32)) -> Option<gsk::RenderNode> {
    if let Some(texture) = node.downcast_ref::<gsk::TextureNode>() {
        return plain_texture(texture, scale);
    }
    if let Some(container) = node.downcast_ref::<gsk::ContainerNode>() {
        let children: Vec<gsk::RenderNode> = (0..container.n_children()).map(|index| container.child(index)).collect();
        let changed: Vec<Option<gsk::RenderNode>> = children.iter().map(|child| rewritten(child, scale)).collect();
        if changed.iter().all(Option::is_none) {
            return None;
        }
        let children: Vec<gsk::RenderNode> = children.into_iter().zip(changed).map(|(old, new)| new.unwrap_or(old)).collect();
        return Some(gsk::ContainerNode::new(&children).upcast());
    }
    if let Some(transform) = node.downcast_ref::<gsk::TransformNode>() {
        let matrix = transform.transform();
        let child = rewritten(&transform.child(), scale_below(&matrix, scale)?)?;
        return Some(gsk::TransformNode::new(child, Some(&matrix)).upcast());
    }
    if let Some(clip) = node.downcast_ref::<gsk::ClipNode>() {
        return Some(gsk::ClipNode::new(rewritten(&clip.child(), scale)?, &clip.clip()).upcast());
    }
    if let Some(clip) = node.downcast_ref::<gsk::RoundedClipNode>() {
        return Some(gsk::RoundedClipNode::new(rewritten(&clip.child(), scale)?, &clip.clip()).upcast());
    }
    if let Some(opacity) = node.downcast_ref::<gsk::OpacityNode>() {
        return Some(gsk::OpacityNode::new(rewritten(&opacity.child(), scale)?, opacity.opacity()).upcast());
    }
    if let Some(debug) = node.downcast_ref::<gsk::DebugNode>() {
        return Some(gsk::DebugNode::new(rewritten(&debug.child(), scale)?, debug.message()).upcast());
    }
    None
}

/// `node` as [`PlainScaleBin`] draws it on a surface with `scale` pixels per unit. Nodes of other
/// kinds than the ones a video widget draws through are returned as they are.
pub fn rewrite(node: &gsk::RenderNode, scale: (f32, f32)) -> gsk::RenderNode {
    if !usable(scale) {
        return node.clone();
    }
    rewritten(node, scale).unwrap_or_else(|| node.clone())
}

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct PlainScaleBin {
        pub child: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlainScaleBin {
        const NAME: &'static str = "TesselPlainScaleBin";
        type Type = super::PlainScaleBin;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for PlainScaleBin {
        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for PlainScaleBin {
        fn compute_expand(&self, hexpand: &mut bool, vexpand: &mut bool) {
            if let Some(child) = self.child.borrow().as_ref() {
                *hexpand = child.compute_expand(gtk::Orientation::Horizontal);
                *vexpand = child.compute_expand(gtk::Orientation::Vertical);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(child) = self.child.borrow().clone() else { return };
            let widget = self.obj();
            let drawn = gtk::Snapshot::new();
            widget.snapshot_child(&child, &drawn);
            let Some(node) = drawn.to_node() else { return };
            let scale = widget.surface_scale();
            snapshot.append_node(rewrite(&node, (scale, scale)));
        }
    }
}

glib::wrapper! {
    /// Holds one child (a `Gtk.Video`, or anything that shows video frames) and draws it without
    /// GTK's full-size copy of strongly reduced frames. Sizes, expands and focuses as its child.
    pub struct PlainScaleBin(ObjectSubclass<imp::PlainScaleBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PlainScaleBin {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let bin: Self = glib::Object::new();
        child.set_parent(&bin);
        bin.imp().child.replace(Some(child.clone().upcast()));
        bin
    }

    pub fn child(&self) -> Option<gtk::Widget> {
        self.imp().child.borrow().clone()
    }

    /// Pixels per unit on the surface this is shown on (fractional scales included).
    fn surface_scale(&self) -> f32 {
        let surface = self.native().and_then(|native| native.surface());
        surface.map_or(self.scale_factor() as f64, |surface| surface.scale()) as f32
    }
}

/// How many texture and scaled-texture nodes `node` holds anywhere inside: `(plain, scaled)`.
/// Reads GTK's own text form of the node, so no kind of node hides its children. For tests.
pub fn count_textures(node: &gsk::RenderNode) -> (usize, usize) {
    let text = node.serialize();
    let text = String::from_utf8_lossy(&text);
    // A node opens as `texture {` or, inside another node, as `child: texture {`.
    let opened = |name: &str| text.lines().filter(|line| {
        let mut words = line.split_whitespace().rev();
        words.next() == Some("{") && words.next() == Some(name)
    }).count();
    (opened("texture"), opened("texture-scale"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::gdk;

    fn texture(width: i32, height: i32) -> gdk::Texture {
        let bytes = glib::Bytes::from_owned(vec![0u8; (width * height * 4) as usize]);
        gdk::MemoryTexture::new(width, height, gdk::MemoryFormat::R8g8b8a8, &bytes, (width * 4) as usize).upcast()
    }

    /// The very same node, not an equal one.
    fn same(a: &gsk::RenderNode, b: &gsk::RenderNode) -> bool {
        use gtk::glib::translate::ToGlibPtr;
        let (a, b): (*mut gsk::ffi::GskRenderNode, *mut gsk::ffi::GskRenderNode) = (a.to_glib_none().0, b.to_glib_none().0);
        a == b
    }

    fn frame(width: i32, height: i32, shown: (f32, f32)) -> gsk::RenderNode {
        gsk::TextureNode::new(&texture(width, height), &graphene::Rect::new(10.0, 20.0, shown.0, shown.1)).upcast()
    }

    #[test]
    fn mipmaps_are_expected_past_a_reduction_of_two() {
        assert!(needs_plain_scale((1920, 1080), (300.0, 169.0), (1.0, 1.0)));
        assert!(needs_plain_scale((1920, 1080), (300.0, 169.0), (2.0, 2.0)));
        assert!(!needs_plain_scale((1920, 1080), (1280.0, 720.0), (1.0, 1.0)));
        assert!(!needs_plain_scale((1920, 1080), (640.0, 360.0), (2.0, 2.0)));
        // One axis is enough, as in GTK.
        assert!(needs_plain_scale((1920, 100), (300.0, 100.0), (1.0, 1.0)));
        // Just under GTK's limit of 2 already counts, so rounding cannot slip a copy through.
        assert!(needs_plain_scale((1950, 100), (1000.0, 100.0), (1.0, 1.0)));
    }

    #[test]
    fn a_strongly_reduced_frame_becomes_a_linear_scaled_texture() {
        let node = rewrite(&frame(1920, 1080, (300.0, 169.0)), (1.0, 1.0));
        let scaled = node.downcast_ref::<gsk::TextureScaleNode>().expect("scaled texture");
        assert_eq!(scaled.filter(), gsk::ScalingFilter::Linear);
        assert_eq!((scaled.texture().width(), scaled.texture().height()), (1920, 1080));
        let bounds = node.bounds();
        assert_eq!((bounds.x(), bounds.y(), bounds.width(), bounds.height()), (10.0, 20.0, 300.0, 169.0));
    }

    #[test]
    fn on_a_scaled_screen_the_node_is_laid_out_in_pixels() {
        let node = rewrite(&frame(3840, 2160, (300.0, 169.0)), (1.5, 1.5));
        let shrink = node.downcast_ref::<gsk::TransformNode>().expect("transform");
        let child = shrink.child();
        assert!(child.is::<gsk::TextureScaleNode>());
        let pixels = child.bounds();
        assert_eq!((pixels.x(), pixels.y(), pixels.width(), pixels.height()), (15.0, 30.0, 450.0, 253.5));
        // The whole thing still covers the place the frame had.
        let bounds = node.bounds();
        assert!((bounds.x() - 10.0).abs() < 0.01 && (bounds.y() - 20.0).abs() < 0.01);
        assert!((bounds.width() - 300.0).abs() < 0.01 && (bounds.height() - 169.0).abs() < 0.01);
    }

    #[test]
    fn a_frame_gtk_would_not_copy_is_left_as_it_is() {
        let original = frame(1920, 1080, (1280.0, 720.0));
        let node = rewrite(&original, (1.0, 1.0));
        assert!(same(&node, &original));
        assert_eq!(count_textures(&node), (1, 0));
    }

    #[test]
    fn frames_are_found_through_the_nodes_a_video_widget_draws() {
        let big = frame(1920, 1080, (300.0, 169.0));
        let small = frame(64, 64, (64.0, 64.0));
        let clip = graphene::Rect::new(0.0, 0.0, 400.0, 300.0);
        let rounded = gsk::RoundedRect::from_rect(clip, 8.0);
        let moved = gsk::Transform::new().translate(&graphene::Point::new(5.0, 5.0));
        let tree: gsk::RenderNode = gsk::ContainerNode::new(&[
            small.clone(),
            gsk::OpacityNode::new(gsk::RoundedClipNode::new(gsk::ClipNode::new(gsk::TransformNode::new(
                gsk::DebugNode::new(big, "video".into()), Some(&moved)), &clip), &rounded), 0.5).upcast(),
        ]).upcast();
        assert_eq!(count_textures(&tree), (2, 0));
        let node = rewrite(&tree, (1.0, 1.0));
        assert_eq!(count_textures(&node), (1, 1));
        assert_eq!(node.bounds(), tree.bounds());
        // The untouched sibling is the same node, not a copy.
        assert!(same(&node.downcast_ref::<gsk::ContainerNode>().unwrap().child(0), &small));
    }

    #[test]
    fn a_scale_transform_counts_towards_the_reduction() {
        let half = gsk::Transform::new().scale(0.5, 0.5);
        // 1000 px shown in 600 units is fine alone, but not once the transform halves it.
        let tree: gsk::RenderNode = gsk::TransformNode::new(frame(1000, 1000, (600.0, 600.0)), Some(&half)).upcast();
        assert_eq!(count_textures(&rewrite(&tree, (1.0, 1.0))), (0, 1));
        let alone = frame(1000, 1000, (600.0, 600.0));
        assert_eq!(count_textures(&rewrite(&alone, (1.0, 1.0))), (1, 0));
    }

    #[test]
    fn a_turned_frame_is_still_found() {
        let turn = gsk::Transform::new().rotate(90.0);
        let tree: gsk::RenderNode = gsk::TransformNode::new(frame(1920, 1080, (300.0, 169.0)), Some(&turn)).upcast();
        assert_eq!(count_textures(&rewrite(&tree, (2.0, 2.0))), (0, 1));
    }

    #[test]
    fn what_cannot_be_judged_is_left_alone() {
        let tilt = gsk::Transform::new().perspective(100.0).rotate_3d(30.0, &graphene::Vec3::y_axis());
        let tree: gsk::RenderNode = gsk::TransformNode::new(frame(1920, 1080, (300.0, 169.0)), Some(&tilt)).upcast();
        assert!(same(&rewrite(&tree, (1.0, 1.0)), &tree));
        let plain = frame(1920, 1080, (300.0, 169.0));
        assert!(same(&rewrite(&plain, (0.0, 0.0)), &plain));
        assert!(same(&rewrite(&plain, (f32::NAN, 1.0)), &plain));
        let color: gsk::RenderNode = gsk::ColorNode::new(&gtk::gdk::RGBA::RED, &graphene::Rect::new(0.0, 0.0, 10.0, 10.0)).upcast();
        assert!(same(&rewrite(&color, (1.0, 1.0)), &color));
    }
}
