//! Protocol attribution for task-local transcript windows.

use crate::tasks::{TaskId, TaskObservation, TaskRuntime};

type Owner = (TaskId, u64);

#[derive(Default)]
pub(super) struct TaskTranscript {
    input: Option<Fragment>,
    output: Option<Fragment>,
}

struct Fragment {
    owner: Option<Owner>,
    text: String,
}

pub(super) fn foreground(runtime: &TaskRuntime) -> Option<Owner> {
    let snapshot = runtime.snapshot();
    let id = snapshot.foreground?;
    snapshot
        .tasks
        .iter()
        .find(|task| task.id == id)
        .map(|task| (id, task.revision))
}

impl TaskTranscript {
    pub(super) fn input(&mut self, text: &str, owner: Option<Owner>) {
        Self::append(&mut self.input, text, owner);
    }

    pub(super) fn output(&mut self, text: &str, owner: Option<Owner>) {
        Self::append(&mut self.output, text, owner);
    }

    fn append(fragment: &mut Option<Fragment>, text: &str, owner: Option<Owner>) {
        if text.is_empty() {
            return;
        }
        fragment
            .get_or_insert_with(|| Fragment {
                owner,
                text: String::new(),
            })
            .text
            .push_str(text);
    }

    /// A successful model routing call can claim its current unowned input once.
    /// UI commands never call this: they have no causal user-utterance token.
    pub(super) fn claim_input(&mut self, owner: Owner) {
        if let Some(input) = &mut self.input
            && input.owner.is_none()
        {
            input.owner = Some(owner);
        }
    }

    pub(super) fn has_input_for(&self, task: &TaskId, revision: u64) -> bool {
        self.input.as_ref().is_some_and(|input| {
            input
                .owner
                .as_ref()
                .is_some_and(|(id, rev)| id == task && *rev == revision)
        })
    }

    pub(super) fn interrupt(&mut self, heard_chars: Option<usize>) -> Option<Owner> {
        let output = self.output.as_mut()?;
        if let Some(chars) = heard_chars {
            let keep = crate::live::playback::heard_prefix(&output.text, chars);
            output.text.truncate(keep);
        }
        output.owner.clone()
    }

    pub(super) fn observations(&self, generation: bool) -> Vec<(Owner, TaskObservation)> {
        let mut windows: Vec<(Owner, String, String)> = Vec::new();
        if let Some(input) = &self.input
            && let Some(owner) = &input.owner
        {
            windows.push((owner.clone(), input.text.clone(), String::new()));
        }
        if let Some(output) = &self.output
            && let Some(owner) = &output.owner
        {
            if let Some((_, _, model)) = windows.iter_mut().find(|(id, _, _)| id == owner) {
                *model = output.text.clone();
            } else {
                windows.push((owner.clone(), String::new(), output.text.clone()));
            }
        }
        windows
            .into_iter()
            .map(|(owner, user, model)| {
                let observation = if generation {
                    TaskObservation::GenerationComplete { user, model }
                } else {
                    TaskObservation::Turn { user, model }
                };
                (owner, observation)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(id: &str, revision: u64) -> Owner {
        (TaskId(id.into()), revision)
    }

    #[test]
    fn switching_before_output_does_not_reassign_input() {
        let mut transcript = TaskTranscript::default();
        transcript.input("first ", Some(owner("billing", 1)));
        transcript.input("request", Some(owner("faq", 1)));
        transcript.output("policy", Some(owner("faq", 1)));
        let observations = transcript.observations(false);
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].0, owner("billing", 1));
        assert!(
            matches!(&observations[0].1, TaskObservation::Turn { user, model }
            if user == "first request" && model.is_empty())
        );
        assert_eq!(observations[1].0, owner("faq", 1));
        assert!(
            matches!(&observations[1].1, TaskObservation::Turn { user, model }
            if user.is_empty() && model == "policy")
        );
    }

    #[test]
    fn unowned_input_requires_an_explicit_single_claim() {
        let mut transcript = TaskTranscript::default();
        transcript.input("book a table", None);
        transcript.output("welcome", Some(owner("booking", 1)));
        assert!(matches!(&transcript.observations(false)[0].1,
            TaskObservation::Turn { user, .. } if user.is_empty()));
        transcript.claim_input(owner("booking", 1));
        transcript.claim_input(owner("other", 1));
        let observations = transcript.observations(false);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].0, owner("booking", 1));
        assert!(
            matches!(&observations[0].1, TaskObservation::Turn { user, .. }
            if user == "book a table")
        );
    }

    #[test]
    fn interruption_trims_heard_output_after_generation_snapshot() {
        let mut transcript = TaskTranscript::default();
        transcript.input("yes", Some(owner("booking", 2)));
        transcript.output("éclair chaud", Some(owner("booking", 2)));
        let generated = transcript.observations(true);
        assert_eq!(transcript.interrupt(Some(8)), Some(owner("booking", 2)));
        assert!(
            matches!(&generated[0].1, TaskObservation::GenerationComplete { model, .. }
            if model == "éclair chaud")
        );
        assert!(matches!(&transcript.observations(false)[0].1,
            TaskObservation::Turn { model, .. } if model == "éclair"));
    }

    #[test]
    fn revision_change_does_not_inherit_old_transcript() {
        let mut transcript = TaskTranscript::default();
        transcript.input("old", Some(owner("booking", 1)));
        transcript.output("new", Some(owner("booking", 2)));
        let observations = transcript.observations(false);
        assert_eq!(observations.len(), 2);
        assert_ne!(observations[0].0, observations[1].0);
    }
}
