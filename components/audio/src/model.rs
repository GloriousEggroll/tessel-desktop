//! Audio graph data shared by the native backend and views (audio_model.py).

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub index: i64,
    pub label: String,
    pub available: bool,
}

impl Choice {
    pub fn new(index: i64, label: &str, available: bool) -> Self {
        Self { index, label: label.into(), available }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: i64,
    pub serial: i64,
    pub name: String,
    pub description: String,
    pub media_class: String,
    pub icon: String,
    pub application: String,
    pub media_name: String,
    pub volume: f64,
    pub mute: bool,
    pub channels: Vec<String>,
    pub channel_volumes: Vec<f64>,
    pub device_id: Option<i64>,
    pub route_index: Option<i64>,
    pub routes: Vec<Choice>,
    pub target_id: Option<i64>,
    pub state: String,
    pub volume_available: bool,
    pub loopback_id: Option<String>,
    pub loopback_internal: bool,
}

impl Node {
    /// `Node(id, serial, name, description, media_class)` with the dataclass defaults.
    pub fn new(id: i64, serial: i64, name: &str, description: &str, media_class: &str) -> Self {
        Self {
            id, serial, name: name.into(), description: description.into(), media_class: media_class.into(),
            icon: "audio-card-symbolic".into(), application: String::new(), media_name: String::new(),
            volume: 1.0, mute: false, channels: Vec::new(), channel_volumes: Vec::new(), device_id: None,
            route_index: None, routes: Vec::new(), target_id: None, state: "unknown".into(),
            volume_available: true, loopback_id: None, loopback_internal: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: i64,
    pub serial: i64,
    pub name: String,
    pub description: String,
    pub profiles: Vec<Choice>,
    pub profile_index: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Loopback {
    pub id: String,
    pub name: String,
    pub target_name: String,
    pub target_description: String,
}

impl Loopback {
    pub fn new(id: &str, name: &str, target_name: &str, target_description: &str) -> Self {
        Self { id: id.into(), name: name.into(), target_name: target_name.into(), target_description: target_description.into() }
    }
}

/// Suspended devices are usable; an explicitly unavailable port is not.
pub fn target_available(node: Option<&Node>) -> bool {
    let Some(node) = node else { return false };
    if node.state == "error" {
        return false;
    }
    node.routes.iter().find(|route| Some(route.index) == node.route_index).is_none_or(|route| route.available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspended_is_available_but_unavailable_route_is_not() {
        let mut node = Node::new(1, 1, "a", "A", "Audio/Sink");
        node.state = "suspended".into();
        node.route_index = Some(4);
        node.routes = vec![Choice::new(4, "Headphones", true)];
        assert!(target_available(Some(&node)));
        node.routes[0].available = false;
        assert!(!target_available(Some(&node)));
        node.routes[0].available = true;
        node.state = "error".into();
        assert!(!target_available(Some(&node)));
        assert!(!target_available(None));
    }
}
