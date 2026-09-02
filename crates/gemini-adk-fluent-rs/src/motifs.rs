//! Conversational motifs — a standard library of high-confidence flow fragments.
//!
//! Most developers don't want to invent flow topology; they want vetted
//! primitives: *collect these slots*, *confirm then commit*, *require a
//! disclosure*, *answer an FAQ then resume*, *hand off*. A [`Motif`] is a factory
//! for a pre-configured [`StageSpec`] (or [`OverlaySpec`]) that you append with
//! [`Conversation::add_stage`](crate::conversation::Conversation::add_stage) /
//! [`add_overlay`](crate::conversation::Conversation::add_overlay) and wire with
//! `.next(..)` as usual. Motifs are *just* lowered through the validated IR — same
//! fail-loud guarantees as hand-written stages (a mis-built commit motif fails
//! `compile()` exactly like a hand-written one would).
//!
//! ```ignore
//! // `ignore`: `Booking` is an application `#[derive(Frame)]` type; the tests
//! // in this module build the same conversation against a concrete frame.
//! let convo = Conversation::new("booking")
//!     .add_stage(Motif::collect_frame::<Booking>("collect"))
//!         .next("confirm", Guard::captured(["party_size"]))
//!     .add_stage(Motif::confirm_then_commit("confirm", "book", "user_confirmed"))
//!         .next("done", Guard::called_ok("book"))
//!     .add_stage(Motif::handoff("done"))
//!     .require(["done"])
//!     .compile()?;
//! ```

use gemini_adk_rs::flow::Guard;
use gemini_adk_rs::frame::Frame;

use crate::conversation::{CommitSpec, OverlaySpec, Resume, StageSpec, TransitionSpec};

/// Namespace of conversational motif factories.
pub struct Motif;

impl Motif {
    /// A stage that collects a typed frame's slots (completes on `captured`).
    pub fn collect_frame<F: Frame>(id: impl Into<String>) -> StageSpec {
        let frame = F::frame();
        StageSpec {
            id: id.into(),
            collect: frame.slot_keys(),
            frame: Some(frame),
            ..Default::default()
        }
    }

    /// A stage that announces something and advances immediately.
    pub fn say(id: impl Into<String>, text: impl Into<String>) -> StageSpec {
        StageSpec {
            id: id.into(),
            say: Some(text.into()),
            done: Some(Guard::always()),
            ..Default::default()
        }
    }

    /// A stage that requires a disclosure acknowledgement before advancing.
    pub fn disclosure(id: impl Into<String>, ack_key: impl Into<String>) -> StageSpec {
        let ack = ack_key.into();
        StageSpec {
            id: id.into(),
            say: Some("Read the required disclosure, then continue.".into()),
            done: Some(Guard::is_true(ack)),
            ..Default::default()
        }
    }

    /// A confirm-before-act stage: `tool` is allowed here, gated behind
    /// `confirm_key`, and the stage completes once `tool` succeeds.
    pub fn confirm_then_commit(
        id: impl Into<String>,
        tool: impl Into<String>,
        confirm_key: impl Into<String>,
    ) -> StageSpec {
        let tool = tool.into();
        StageSpec {
            id: id.into(),
            allow: vec![tool.clone()],
            commit: Some(CommitSpec {
                tool: tool.clone(),
                when: Guard::is_true(confirm_key),
            }),
            done: Some(Guard::called_ok(tool)),
            ..Default::default()
        }
    }

    /// An identity-verification stage: completes once `verified_key` is true.
    pub fn identity_verification(
        id: impl Into<String>,
        verified_key: impl Into<String>,
    ) -> StageSpec {
        StageSpec {
            id: id.into(),
            say: Some("Verify the caller's identity before proceeding.".into()),
            done: Some(Guard::is_true(verified_key)),
            ..Default::default()
        }
    }

    /// A terminal handoff stage (optionally allowing a transfer tool).
    pub fn handoff(id: impl Into<String>) -> StageSpec {
        StageSpec {
            id: id.into(),
            say: Some("Hand off to a human agent with a summary.".into()),
            terminal: true,
            ..Default::default()
        }
    }

    /// An FAQ digression: triggered by `trigger_key`, it answers a side question
    /// (gated on `answered_key`) and resumes the main flow where it left off.
    pub fn faq_digression(
        name: impl Into<String>,
        trigger_key: impl Into<String>,
        answered_key: impl Into<String>,
    ) -> OverlaySpec {
        let answered = answered_key.into();
        OverlaySpec {
            name: name.into(),
            trigger: Guard::is_true(trigger_key),
            stages: vec![
                StageSpec {
                    id: "answer".into(),
                    say: Some("Answer the user's question.".into()),
                    done: Some(Guard::is_true(answered.clone())),
                    next: vec![TransitionSpec {
                        to: "faq_end".into(),
                        when: Guard::is_true(answered),
                    }],
                    ..Default::default()
                },
                StageSpec {
                    id: "faq_end".into(),
                    terminal: true,
                    ..Default::default()
                },
            ],
            require: Vec::new(),
            resume: Resume::Previous,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Conversation;
    use crate::simulation::Sim;
    use gemini_adk_rs::flow::Enforcement;
    use gemini_adk_rs::frame::{Frame, FrameSpec, SlotRecognizer, SlotSpec};

    struct Booking;
    impl Frame for Booking {
        fn frame() -> FrameSpec {
            FrameSpec {
                name: "booking".into(),
                slots: vec![SlotSpec {
                    recognizer: Some(SlotRecognizer::IntegerNear(vec!["people".into()])),
                    ..SlotSpec::new("party_size")
                }],
            }
        }
    }

    #[tokio::test]
    async fn motifs_compose_into_a_runnable_conversation() {
        let convo = Conversation::new("booking")
            .add_stage(Motif::identity_verification("verify", "verified"))
            .next("collect", Guard::is_true("verified"))
            .add_stage(Motif::collect_frame::<Booking>("collect"))
            .next("confirm", Guard::captured(["party_size"]))
            .add_stage(Motif::confirm_then_commit(
                "confirm",
                "book",
                "user_confirmed",
            ))
            .next("done", Guard::called_ok("book"))
            .add_stage(Motif::handoff("done"))
            .require(["done"])
            .compile()
            .expect("motif conversation compiles");

        let mut sim = Sim::new(&convo, Enforcement::Enforce);
        assert!(sim.active().contains(&"verify".to_string()));

        sim.set("verified", true);
        sim.turn();
        assert!(sim.active().contains(&"collect".to_string()));

        sim.user("a table for 4 people").await;
        assert_eq!(sim.slot::<u32>("party_size"), Some(4));
        assert!(sim.active().contains(&"confirm".to_string()));

        // confirm_then_commit gates `book` behind confirmation.
        assert!(!sim.allowed("book"));
        sim.set("user_confirmed", true);
        sim.turn();
        assert!(sim.allowed("book"));
        sim.tool_ok("book");
        assert!(sim.is_complete());
    }

    #[tokio::test]
    async fn faq_digression_motif_suspends_and_resumes() {
        let convo = Conversation::new("support")
            .stage("triage")
            .next("resolve", Guard::is_true("triaged"))
            .stage("resolve")
            .terminal()
            .add_overlay(Motif::faq_digression("faq", "intent:faq", "faq_answered"))
            .compile()
            .expect("compiles");

        let mut sim = Sim::new(&convo, Enforcement::Enforce);
        assert!(sim.active().contains(&"triage".to_string()));

        sim.set("intent:faq", true);
        sim.turn();
        assert_eq!(sim.active_overlay(), Some("faq"));

        sim.set("faq_answered", true);
        sim.set("intent:faq", false);
        sim.turn();
        assert!(sim.active_overlay().is_none());
        assert!(sim.active().contains(&"triage".to_string())); // resumed where it was
    }

    #[test]
    fn unguarded_confirm_motif_would_be_rejected() {
        // Sanity: motifs lower through the validated IR. A confirm stage whose
        // commit guard is always-true is rejected just like a hand-written one.
        let mut stage = Motif::confirm_then_commit("confirm", "book", "user_confirmed");
        stage.commit = Some(CommitSpec {
            tool: "book".into(),
            when: Guard::always(),
        });
        let err = Conversation::new("x")
            .add_stage(stage)
            .next("done", Guard::called_ok("book"))
            .stage("done")
            .terminal()
            .compile()
            .expect_err("unguarded commit must fail");
        assert!(matches!(
            err,
            crate::conversation::ConversationError::Compile(_)
        ));
    }
}
