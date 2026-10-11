// One Jev decision through Vercel AI Gateway with the AI SDK: the three
// question types the Gemini Live runtime uses, asked about one caller turn.
//
//   npm run decide        (reads AI_GATEWAY_API_KEY from ../../.env.local)
import { experimental_decide as decide } from 'ai';

if (!process.env.AI_GATEWAY_API_KEY && !process.env.VERCEL_OIDC_TOKEN) {
  console.error('Set AI_GATEWAY_API_KEY in .env.local at the repository root.');
  process.exit(1);
}

const state = {
  stage: 'confirm',
  readBack: 'Party of 4 at 7 pm tomorrow, under Rossi.',
  turns: [
    { agent: 'Four at seven tomorrow under Rossi. Shall I book it?' },
    { caller: 'Yes, that is all correct. Please book it.' },
  ],
};

const started = performance.now();
try {
  const result = await decide({
    model: 'typesafe-ai/jev',
    state,
    questions: {
      confirmed: {
        type: 'boolean',
        instructions: 'In their last turn, did the caller agree to go ahead with the booking that was read back?',
        criteria: {
          true: 'the caller said yes to the read-back in their own words',
          false: 'the caller hesitated, changed a detail, asked something, or only picked an option',
        },
      },
      next: {
        type: 'choice',
        instructions: 'What should the conversation do next?',
        criteria: {
          book: 'the caller confirmed: make the booking',
          read_back_again: 'the caller changed or questioned a detail',
          handoff: 'the caller asked for a person',
          stay: 'nothing decided yet',
        },
      },
      frustration: {
        type: 'score',
        instructions: 'How frustrated does the caller sound?',
        criteria: ['calm', 'slightly impatient', 'frustrated', 'angry'],
      },
    },
  });
  const ms = Math.round(performance.now() - started);
  console.log(JSON.stringify({ ms, answers: result.answers, usage: result.usage }, null, 2));
} catch (error) {
  const ms = Math.round(performance.now() - started);
  console.error(`decide failed after ${ms} ms: ${error?.name ?? 'Error'}: ${error?.message ?? error}`);
  process.exit(1);
}
