/**
 * panels/artifact-panel.js — Artifact browser with version history.
 *
 * Displays artifacts stored during agent execution, with version
 * tracking and content preview. Supports text, JSON, and binary artifacts.
 *
 * Contract: create(container) / addArtifact(artifact) / reset()
 */
var ArtifactPanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  function ArtifactPanel() {
    this._container = null;
    this._artifacts = {};  // name -> [{ version, content, mime_type, timestamp }]
    this._listEl = null;
    this._previewEl = null;
    this._selectedName = null;
    this._selectedVersion = null;
  }

  ArtifactPanel.prototype.create = function (container) {
    this._container = container;
    container.className = 'devtools-panel artifact-panel';
    this._build();
  };

  ArtifactPanel.prototype._build = function () {
    var c = this._container;
    c.innerHTML = '';

    // Header
    var header = U.el('div', 'artifact-header');
    var title = U.el('span', 'artifact-title');
    title.textContent = 'Artifacts';
    header.appendChild(title);

    var countBadge = U.el('span', 'artifact-count');
    countBadge.textContent = '0';
    header.appendChild(countBadge);
    this._countBadge = countBadge;

    c.appendChild(header);

    // Split: list left, preview right
    var split = U.el('div', 'artifact-split');

    this._listEl = U.el('div', 'artifact-list');
    split.appendChild(this._listEl);

    this._previewEl = U.el('div', 'artifact-preview');
    this._previewEl.innerHTML = '<div class="artifact-preview-empty">Select an artifact</div>';
    split.appendChild(this._previewEl);

    c.appendChild(split);

    this._renderList();
  };

  ArtifactPanel.prototype.addArtifact = function (artifact) {
    var name = artifact.name || artifact.filename || 'unnamed';
    if (!this._artifacts[name]) {
      this._artifacts[name] = [];
    }

    this._artifacts[name].push({
      version: this._artifacts[name].length + 1,
      content: artifact.content || artifact.data || '',
      mime_type: artifact.mime_type || artifact.content_type || 'text/plain',
      timestamp: artifact.timestamp || Date.now(),
      size: artifact.size || (artifact.content || '').length
    });

    this._updateCount();
    this._renderList();
  };

  ArtifactPanel.prototype._updateCount = function () {
    if (this._countBadge) {
      this._countBadge.textContent = Object.keys(this._artifacts).length;
    }
  };

  ArtifactPanel.prototype._renderList = function () {
    if (!this._listEl) return;
    this._listEl.innerHTML = '';

    var names = Object.keys(this._artifacts).sort();
    var self = this;

    if (names.length === 0) {
      var empty = U.el('div', 'artifact-list-empty');
      empty.textContent = 'No artifacts yet';
      this._listEl.appendChild(empty);
      return;
    }

    names.forEach(function (name) {
      var versions = self._artifacts[name];
      var latest = versions[versions.length - 1];

      var row = U.el('div', 'artifact-row' + (name === self._selectedName ? ' selected' : ''));

      var icon = U.el('span', 'artifact-icon');
      icon.textContent = _mimeIcon(latest.mime_type);
      row.appendChild(icon);

      var info = U.el('div', 'artifact-info');

      var nameEl = U.el('div', 'artifact-name');
      nameEl.textContent = U.truncText(name, 30);
      nameEl.title = name;
      info.appendChild(nameEl);

      var meta = U.el('div', 'artifact-meta');
      meta.textContent = 'v' + versions.length + ' \u2022 ' + _formatSize(latest.size) + ' \u2022 ' + latest.mime_type;
      info.appendChild(meta);

      row.appendChild(info);

      row.addEventListener('click', function () {
        self._selectedName = name;
        self._selectedVersion = versions.length;
        self._renderList();
        self._renderPreview(name, versions.length);
      });

      self._listEl.appendChild(row);
    });
  };

  ArtifactPanel.prototype._renderPreview = function (name, versionNum) {
    var d = this._previewEl;
    d.innerHTML = '';

    var versions = this._artifacts[name];
    if (!versions) return;

    var version = versions[versionNum - 1];
    if (!version) return;

    // Header with version selector
    var header = U.el('div', 'artifact-preview-header');

    var titleEl = U.el('div', 'artifact-preview-title');
    titleEl.textContent = name;
    header.appendChild(titleEl);

    if (versions.length > 1) {
      var versionSel = U.el('select', 'artifact-version-select');
      var self = this;
      for (var i = versions.length; i >= 1; i--) {
        var opt = U.el('option', '');
        opt.value = i;
        opt.textContent = 'v' + i;
        if (i === versionNum) opt.selected = true;
        versionSel.appendChild(opt);
      }
      versionSel.addEventListener('change', function () {
        self._selectedVersion = parseInt(this.value);
        self._renderPreview(name, self._selectedVersion);
      });
      header.appendChild(versionSel);
    }

    d.appendChild(header);

    // Metadata
    var metaEl = U.el('div', 'artifact-preview-meta');
    metaEl.textContent = 'Type: ' + version.mime_type + ' | Size: ' + _formatSize(version.size);
    d.appendChild(metaEl);

    // Content
    var contentEl = U.el('div', 'artifact-preview-content');

    if (version.mime_type.indexOf('json') !== -1) {
      var pre = U.el('pre', 'artifact-json');
      try {
        pre.textContent = JSON.stringify(JSON.parse(version.content), null, 2);
      } catch (e) {
        pre.textContent = version.content;
      }
      contentEl.appendChild(pre);
    } else if (version.mime_type.indexOf('image') !== -1) {
      var img = U.el('div', 'artifact-image-placeholder');
      img.textContent = '[Binary image: ' + _formatSize(version.size) + ']';
      contentEl.appendChild(img);
    } else {
      var pre = U.el('pre', 'artifact-text');
      pre.textContent = version.content;
      contentEl.appendChild(pre);
    }

    d.appendChild(contentEl);

    // Version diff indicator for v2+
    if (versionNum > 1) {
      var diffNote = U.el('div', 'artifact-diff-note');
      diffNote.textContent = 'Updated from v' + (versionNum - 1);
      d.appendChild(diffNote);
    }
  };

  ArtifactPanel.prototype.reset = function () {
    this._artifacts = {};
    this._selectedName = null;
    this._selectedVersion = null;
    if (this._listEl) this._listEl.innerHTML = '';
    if (this._previewEl) {
      this._previewEl.innerHTML = '<div class="artifact-preview-empty">Select an artifact</div>';
    }
    this._updateCount();
  };

  function _mimeIcon(mime) {
    if (mime.indexOf('json') !== -1) return '\u007B\u007D';
    if (mime.indexOf('image') !== -1) return '\uD83D\uDDBC';
    if (mime.indexOf('audio') !== -1) return '\uD83C\uDFB5';
    if (mime.indexOf('html') !== -1) return '\u003C/\u003E';
    return '\uD83D\uDCC4';
  }

  function _formatSize(bytes) {
    if (bytes < 1024) return bytes + 'B';
    if (bytes < 1048576) return (bytes / 1024).toFixed(1) + 'KB';
    return (bytes / 1048576).toFixed(1) + 'MB';
  }

  return ArtifactPanel;
})();
