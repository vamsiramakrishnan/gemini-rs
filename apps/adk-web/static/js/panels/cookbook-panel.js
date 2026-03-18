/**
 * panels/cookbook-panel.js — Cookbook browser panel for the devtools sidebar.
 *
 * Shows the current demo's description, features used, source code path,
 * and difficulty tier. Populated from the appMeta message.
 *
 * Contract: create(container) / setAppMeta(info) / reset()
 */
var CookbookPanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  // -- Static metadata about each demo --

  var COOKBOOK_DATA = {
    'text-chat': {
      description: 'Minimal text chat agent. Sends user text to Gemini and streams back the response. No tools, no phases -- the simplest possible integration.',
      source: 'examples/text-chat/src/main.rs',
      features: ['Text', 'Streaming'],
      difficulty: 'crawl'
    },
    'voice-chat': {
      description: 'Real-time voice conversation using the Gemini Live API. Captures microphone PCM, streams it over WebSocket, and plays back audio responses with jitter buffering.',
      source: 'examples/voice-chat/src/main.rs',
      features: ['Voice', 'Audio', 'Streaming', 'VAD'],
      difficulty: 'crawl'
    },
    'tool-calling': {
      description: 'Demonstrates function calling with SimpleTool and TypedTool. The model can invoke tools, receive results, and incorporate them into its response.',
      source: 'examples/tool-calling/src/main.rs',
      features: ['Tools', 'TypedTool', 'SimpleTool'],
      difficulty: 'crawl'
    },
    'restaurant': {
      description: 'Multi-phase restaurant order assistant. Uses the phase system to guide the conversation through greeting, order-taking, confirmation, and farewell stages.',
      source: 'apps/adk-web/src/apps/restaurant.rs',
      features: ['Phases', 'State Machine', 'Transitions', 'Guards', 'Extractors'],
      difficulty: 'walk'
    },
    'extractors': {
      description: 'Out-of-band extraction pipeline. Runs a secondary LLM call on each turn to extract structured data (JSON schema) from the conversation without interrupting the main flow.',
      source: 'apps/adk-web/src/apps/mod.rs',
      features: ['Extractors', 'Typed Extraction', 'JSON Schema'],
      difficulty: 'walk'
    },
    'playbook': {
      description: 'Playbook-driven agent that follows a structured script. Demonstrates dynamic instructions, phase-level tool gating, and context injection steering.',
      source: 'apps/adk-web/src/apps/mod.rs',
      features: ['Phases', 'Dynamic Instructions', 'Context Injection'],
      difficulty: 'walk'
    },
    'clinic': {
      description: 'Medical clinic intake assistant. Multi-phase flow with needs-based repair, temporal patterns for detecting confused users, and structured data extraction.',
      source: 'apps/adk-web/src/apps/clinic.rs',
      features: ['Phases', 'Repair', 'Temporal Patterns', 'Extractors', 'Needs'],
      difficulty: 'walk'
    },
    'debt-collection': {
      description: 'Production-grade debt collection agent with compliance guardrails, mandatory disclosures, agent-as-tool for verification, and full audit trail.',
      source: 'apps/adk-web/src/apps/debt_collection.rs',
      features: ['Phases', 'Guardrails', 'Agent-as-Tool', 'Watchers', 'Compliance'],
      difficulty: 'run'
    },
    'guardrails': {
      description: 'Demonstrates content guardrails and safety filters. Shows how to gate model behavior, block unsafe topics, and enforce output constraints.',
      source: 'apps/adk-web/src/apps/mod.rs',
      features: ['Guardrails', 'Watchers', 'Content Filtering'],
      difficulty: 'run'
    },
    'support-assistant': {
      description: 'Customer support agent with knowledge base lookup, escalation flow, sentiment tracking, and session persistence for surviving restarts.',
      source: 'apps/adk-web/src/apps/support.rs',
      features: ['Phases', 'Tools', 'Watchers', 'Persistence', 'Temporal Patterns'],
      difficulty: 'run'
    },
    'call-screening': {
      description: 'Inbound call screening agent. Identifies caller intent, routes to the right department, and blocks spam -- all in real-time voice.',
      source: 'apps/adk-web/src/apps/call_screening.rs',
      features: ['Voice', 'Phases', 'Extractors', 'Routing', 'Guards'],
      difficulty: 'run'
    },
    'all-config': {
      description: 'Kitchen-sink demo exercising every configuration option: phases, extractors, watchers, temporal patterns, repair, steering modes, tool advisory, and persistence.',
      source: 'apps/adk-web/src/apps/all_config.rs',
      features: ['Phases', 'Extractors', 'Watchers', 'Temporal Patterns', 'Repair', 'Steering', 'Persistence', 'Tool Advisory'],
      difficulty: 'run'
    }
  };

  var DIFFICULTY_LABELS = {
    crawl: { label: 'Crawl', subtitle: 'Single agent', color: '#1967d2', bg: '#e8f0fe' },
    walk:  { label: 'Walk',  subtitle: 'Multi-agent',  color: '#b06000', bg: '#fef7e0' },
    run:   { label: 'Run',   subtitle: 'Production',   color: '#c5221f', bg: '#fce8e6' }
  };

  var FEATURE_COLORS = {
    'phases':             { bg: '#fef7e0', color: '#b06000' },
    'state machine':      { bg: '#fef7e0', color: '#b06000' },
    'transitions':        { bg: '#fef7e0', color: '#b06000' },
    'guards':             { bg: '#fef7e0', color: '#b06000' },
    'tools':              { bg: '#e6f4ea', color: '#137333' },
    'typedtool':          { bg: '#e6f4ea', color: '#137333' },
    'simpletool':         { bg: '#e6f4ea', color: '#137333' },
    'voice':              { bg: '#e8f0fe', color: '#1967d2' },
    'audio':              { bg: '#f3e8fd', color: '#7627bb' },
    'vad':                { bg: '#f3e8fd', color: '#7627bb' },
    'streaming':          { bg: '#e0f7fa', color: '#00695c' },
    'text':               { bg: '#f1f3f4', color: '#5f6368' },
    'extractors':         { bg: '#fff3e0', color: '#e65100' },
    'typed extraction':   { bg: '#fff3e0', color: '#e65100' },
    'json schema':        { bg: '#fff3e0', color: '#e65100' },
    'guardrails':         { bg: '#fce8e6', color: '#c5221f' },
    'compliance':         { bg: '#fce8e6', color: '#c5221f' },
    'content filtering':  { bg: '#fce8e6', color: '#c5221f' },
    'watchers':           { bg: '#f3e8fd', color: '#7627bb' },
    'temporal patterns':  { bg: '#f3e8fd', color: '#7627bb' },
    'repair':             { bg: '#fce8e6', color: '#c5221f' },
    'needs':              { bg: '#fce8e6', color: '#c5221f' },
    'agent-as-tool':      { bg: '#e6f4ea', color: '#137333' },
    'persistence':        { bg: '#e0f7fa', color: '#00695c' },
    'routing':            { bg: '#e8f0fe', color: '#1967d2' },
    'dynamic instructions': { bg: '#fef7e0', color: '#b06000' },
    'context injection':  { bg: '#fef7e0', color: '#b06000' },
    'steering':           { bg: '#fef7e0', color: '#b06000' },
    'tool advisory':      { bg: '#e6f4ea', color: '#137333' }
  };

  function CookbookPanel() {
    this._container = null;
    this._appInfo = null;
    this._contentEl = null;
  }

  CookbookPanel.prototype.create = function (container) {
    this._container = container;
    container.className = 'devtools-panel cookbook-panel';
    this._build();
  };

  CookbookPanel.prototype._build = function () {
    var c = this._container;
    c.innerHTML = '';

    var contentEl = U.el('div', 'cookbook-content');
    contentEl.innerHTML =
      '<div class="cookbook-empty">' +
        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">' +
          '<path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20"/>' +
          '<path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z"/>' +
        '</svg>' +
        '<div class="cookbook-empty-text">Connect to see cookbook details</div>' +
      '</div>';
    c.appendChild(contentEl);
    this._contentEl = contentEl;
  };

  CookbookPanel.prototype.setAppMeta = function (info) {
    this._appInfo = info;
    this._render();
  };

  CookbookPanel.prototype._render = function () {
    var el = this._contentEl;
    if (!el || !this._appInfo) return;

    var info = this._appInfo;
    var urlName = (info.name || '').toLowerCase().replace(/\s+/g, '-');
    var cookbook = COOKBOOK_DATA[urlName] || null;

    var html = '';

    // Title + difficulty badge
    var diff = cookbook ? cookbook.difficulty : 'crawl';
    var diffMeta = DIFFICULTY_LABELS[diff] || DIFFICULTY_LABELS.crawl;

    html += '<div class="cookbook-header">';
    html += '<h3 class="cookbook-title">' + U.esc(info.name || urlName) + '</h3>';
    html += '<span class="cookbook-difficulty" style="background:' + diffMeta.bg + ';color:' + diffMeta.color + '">';
    html += U.esc(diffMeta.label) + ' <span class="diff-sub">' + U.esc(diffMeta.subtitle) + '</span>';
    html += '</span>';
    html += '</div>';

    // Description
    var desc = cookbook ? cookbook.description : (info.description || 'No description available.');
    html += '<div class="cookbook-section">';
    html += '<div class="cookbook-section-label">About</div>';
    html += '<p class="cookbook-desc">' + U.esc(desc) + '</p>';
    html += '</div>';

    // Features used
    var features = cookbook ? cookbook.features : (info.features || []);
    if (features.length > 0) {
      html += '<div class="cookbook-section">';
      html += '<div class="cookbook-section-label">Features Used</div>';
      html += '<div class="cookbook-tags">';
      features.forEach(function (f) {
        var key = f.toLowerCase();
        var fc = FEATURE_COLORS[key] || { bg: '#f1f3f4', color: '#5f6368' };
        html += '<span class="cookbook-tag" style="background:' + fc.bg + ';color:' + fc.color + '">' + U.esc(f) + '</span>';
      });
      html += '</div>';
      html += '</div>';
    }

    // Source code path
    if (cookbook && cookbook.source) {
      html += '<div class="cookbook-section">';
      html += '<div class="cookbook-section-label">Source</div>';
      html += '<div class="cookbook-source">';
      html += '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" class="cookbook-source-icon">';
      html += '<polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/>';
      html += '</svg>';
      html += '<code class="cookbook-source-path">' + U.esc(cookbook.source) + '</code>';
      html += '</div>';
      html += '</div>';
    }

    // Tips
    if (info.tips && info.tips.length > 0) {
      html += '<div class="cookbook-section">';
      html += '<div class="cookbook-section-label">Tips</div>';
      html += '<ul class="cookbook-tips">';
      info.tips.forEach(function (tip) {
        html += '<li>' + U.esc(tip) + '</li>';
      });
      html += '</ul>';
      html += '</div>';
    }

    // Try saying
    if (info.try_saying && info.try_saying.length > 0) {
      html += '<div class="cookbook-section">';
      html += '<div class="cookbook-section-label">Try Saying</div>';
      html += '<div class="cookbook-try-saying">';
      info.try_saying.forEach(function (phrase) {
        html += '<div class="cookbook-phrase">\u201c' + U.esc(phrase) + '\u201d</div>';
      });
      html += '</div>';
      html += '</div>';
    }

    el.innerHTML = html;
  };

  CookbookPanel.prototype.reset = function () {
    this._appInfo = null;
    this._build();
  };

  return CookbookPanel;
})();
