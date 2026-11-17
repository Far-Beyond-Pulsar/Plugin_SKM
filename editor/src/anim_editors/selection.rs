//! The typed selection every animation editor shares. Selecting anything an
//! editor authors (a bone, notify, sample, graph node, segment, chain, goal,
//! pose-search result) sets one of these, and the properties inspector
//! renders from it. Selection is editor state: it is never written to an
//! asset file.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AnimSelection {
    #[default]
    None,
    Bone {
        id: String,
    },
    Notify {
        id: String,
    },
    NotifyState {
        id: String,
    },
    BlendSample {
        id: String,
    },
    BlendAxis {
        vertical: bool,
    },
    MontageSegment {
        track: String,
        id: String,
    },
    MontageSlot {
        track: String,
    },
    MontageSection {
        id: String,
    },
    GraphNode {
        id: String,
    },
    GraphParameter {
        name: String,
    },
    RetargetChain {
        name: String,
    },
    IkChain {
        name: String,
    },
    IkGoal {
        name: String,
    },
    IkSolver {
        index: usize,
    },
    JointLimit {
        bone: String,
    },
    PoseSource {
        id: String,
    },
    PoseChannel {
        index: usize,
    },
    PoseResult {
        index: usize,
    },
}

impl AnimSelection {
    #[cfg(test)]
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Heading of the inspector for this selection.
    pub fn title(&self) -> &'static str {
        match self {
            Self::None => "Nothing selected",
            Self::Bone { .. } => "Bone",
            Self::Notify { .. } => "Notify",
            Self::NotifyState { .. } => "Notify State",
            Self::BlendSample { .. } => "Blend Sample",
            Self::BlendAxis { .. } => "Axis",
            Self::MontageSegment { .. } => "Segment",
            Self::MontageSlot { .. } => "Slot Track",
            Self::MontageSection { .. } => "Section",
            Self::GraphNode { .. } => "Graph Node",
            Self::GraphParameter { .. } => "Parameter",
            Self::RetargetChain { .. } => "Retarget Chain",
            Self::IkChain { .. } => "IK Chain",
            Self::IkGoal { .. } => "IK Goal",
            Self::IkSolver { .. } => "Solver",
            Self::JointLimit { .. } => "Joint Limit",
            Self::PoseSource { .. } => "Source Clip",
            Self::PoseChannel { .. } => "Feature Channel",
            Self::PoseResult { .. } => "Match",
        }
    }
}
