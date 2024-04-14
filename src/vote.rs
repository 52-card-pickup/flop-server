use crate::state::{self, Motion};

pub(crate) fn player_start_vote(state: &mut state::State, motion: Motion) -> Result<(), String> {
    if state.vote.is_some() {
        return Err("Vote already in progress".to_string());
    }

    let mut vote = state::Vote::default();
    vote.motion = motion;

    vote.end_time.add_ms(60000);

    state.vote = Some(vote);

    Ok(())
}
