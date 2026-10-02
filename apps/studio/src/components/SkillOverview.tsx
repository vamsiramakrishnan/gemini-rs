import { useStudio } from '../store';
import { isObject } from '../spec/paths';

export function SkillOverview() {
  const spec = useStudio((state) => state.spec);
  const selection = useStudio((state) => state.selection);
  const skills = Array.isArray(spec.skills) ? spec.skills : [];
  const addSkill = () => {
    const names = new Set(skills.filter(isObject).map((skill) => skill.name));
    let suffix = skills.length + 1;
    while (names.has(`skill-${suffix}`)) suffix += 1;
    useStudio.getState().edit({ ...spec, skills: [...skills, { name: `skill-${suffix}`, version: '1', description: '', instruction: '' }] });
    useStudio.getState().select({ kind: 'skill', index: skills.length });
  };
  return <div className="skill-overview">
    <div className="row"><span className="kind">Voice assistant</span><span className="hint">{skills.length} reusable skills</span></div>
    <h1>{typeof spec.name === 'string' ? spec.name : 'Your assistant'}</h1>
    <p className="hint">One conversation, separate tasks. Select a skill to edit its instructions, tools, dialogue, extraction, and memory.</p>
    <div className="row skill-overview-actions">
      <button type="button" className="primary" onClick={() => useStudio.getState().openDock('tasks')}>Try tasks offline</button>
      <button type="button" onClick={addSkill}>Add skill</button>
    </div>
    <ol className="skill-library" aria-label="Authored skills">
      {skills.map((skill, index) => {
        if (!isObject(skill)) return null;
        const tools = Array.isArray(skill.tools) ? skill.tools.filter(isObject) : [];
        const inputs = isObject(skill.inputs) ? Object.keys(skill.inputs) : [];
        return <li key={index}>
          <button type="button" className={selection?.kind === 'skill' && selection.index === index ? 'selected' : ''} onClick={() => useStudio.getState().select({ kind: 'skill', index })}>
            <span className="skill-number">{String(index + 1).padStart(2, '0')}</span>
            <span className="skill-library-description">
              <strong>{typeof skill.name === 'string' ? skill.name : 'Unnamed skill'}</strong>
              <span className="hint">{typeof skill.description === 'string' && skill.description ? skill.description : 'Add a description to explain when to use this skill.'}</span>
              <span className="hint">{inputs.length ? `Inputs: ${inputs.join(', ')} · ` : ''}{tools.length} tool{tools.length === 1 ? '' : 's'}{skill.conversation ? ' · Conversation' : skill.flow ? ' · Governed flow' : ''}</span>
            </span>
            <span className="badge">v{typeof skill.version === 'string' ? skill.version : '?'}</span>
          </button>
        </li>;
      })}
    </ol>
  </div>;
}
