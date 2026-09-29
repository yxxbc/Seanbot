/* ============================================================================
   Seanbot 官网交互（无依赖，原生 JS）
   负责四件事：系统自动识别 → 选中对应安装包；产品切换（CLI / TUI / App）；
   安装命令复制；预览图按产品切换（素材缺失时退回占位）。

   ---------------------------------------------------------------------------
   维护指引
   ---------------------------------------------------------------------------
   1) 补预览图：把文件放到 site/previews/，命名 cli.png / tui.png / app.png，
      刷新页面即可自动显示（加载失败才退回占位）。要改名就改 PRODUCTS[x].preview。
      推荐 1600×1000 PNG、单张 < 1 MiB（仓库 pre-commit 会拦超过 1 MiB 的文件）。
   2) 新增平台或架构：在 TARGETS 里加一项（target 必须是 CI 真实构建的三元组，
      见 .github/workflows/release.yml 的 matrix），页面会自动多出一个系统/架构按钮。
      归档扩展名在 TARGETS[x].archs[].ext 里写死，和 scripts/package.sh 保持一致。
   3) TUI 正式发布后：把 PRODUCTS.tui.status 改成 'available'、statusText 改成 '可用'，
      并把 caption 里的「预发布」去掉即可，其余逻辑不用动。
   ========================================================================== */

(function () {
  'use strict';

  var REPO = 'https://github.com/yxxbc/Seanbot';
  var RELEASES = REPO + '/releases';
  var LATEST_DOWNLOAD = RELEASES + '/latest/download/';
  var INSTALL_SH = 'curl -fsSL https://raw.githubusercontent.com/yxxbc/Seanbot/main/scripts/install.sh | sh';
  var INSTALL_PS1 = 'irm https://raw.githubusercontent.com/yxxbc/Seanbot/main/scripts/install.ps1 | iex';

  /* 产品形态。share=true 表示有可下载的发布包；app 未发布所以是 false。 */
  var PRODUCTS = {
    cli: {
      name: 'CLI',
      pill: '可用',
      pillClass: '',
      statusText: '逐行对话的终端版本，五个平台的发布包都已就绪（预发布）。',
      share: true,
      preview: 'previews/cli.png',
      windowTitle: 'sean — 终端',
      caption: 'CLI：进入后直接输入问题；/help 看命令，Ctrl+C 中断当前任务。'
    },
    tui: {
      name: 'TUI',
      pill: '预发布',
      pillClass: 'pill--pre',
      statusText: '全屏终端界面：斜杠命令浮窗、工具确认、Markdown 渲染、Ctrl+O 转录视图。',
      share: true,
      preview: 'previews/tui.png',
      windowTitle: 'sean — TUI',
      caption: 'TUI：与 CLI 是同一个 sean 可执行文件，安装后运行 sean 即可。'
    },
    app: {
      name: 'App',
      pill: '开发中',
      pillClass: 'pill--wip',
      statusText: '桌面端还没有可下载的版本。',
      share: false,
      preview: 'previews/app.png',
      windowTitle: 'Seanbot — 桌面端',
      caption: 'App：桌面端仍在开发，截图稍后补上。'
    }
  };

  /* 平台 / 架构 → 发布产物。
     target 必须与 .github/workflows/release.yml 的 matrix 以及 scripts/package.sh 的命名一致。 */
  var TARGETS = {
    macos: {
      label: 'macOS',
      detectNote: 'macOS',
      defaultArch: 'arm64',
      archs: [
        { id: 'arm64', label: 'Apple 芯片', target: 'aarch64-apple-darwin', ext: 'tar.gz' },
        { id: 'x64', label: 'Intel', target: 'x86_64-apple-darwin', ext: 'tar.gz' }
      ],
      install: INSTALL_SH,
      installNote: '装到 ~/.local/bin；可用 SEANBOT_VERSION、SEANBOT_INSTALL_DIR 覆盖默认行为。'
    },
    windows: {
      label: 'Windows',
      detectNote: 'Windows',
      defaultArch: 'x64',
      archs: [
        { id: 'x64', label: 'x64', target: 'x86_64-pc-windows-msvc', ext: 'zip' }
      ],
      install: INSTALL_PS1,
      installNote: '在 PowerShell 中运行；ARM 设备用 x64 包（系统自带模拟）。'
    },
    linux: {
      label: 'Linux',
      detectNote: 'Linux',
      defaultArch: 'x64',
      archs: [
        { id: 'x64', label: 'x86_64', target: 'x86_64-unknown-linux-gnu', ext: 'tar.gz' },
        { id: 'arm64', label: 'ARM64', target: 'aarch64-unknown-linux-gnu', ext: 'tar.gz' }
      ],
      install: INSTALL_SH,
      installNote: '构建基于 glibc；安装脚本会自动挑选对应架构的包。'
    }
  };

  var state = { product: 'cli', os: 'macos', arch: 'arm64', detected: false, mobile: false };

  var $ = function (id) { return document.getElementById(id); };
  var els = {};

  /* ---------------------------------------------------------------- 系统识别 */
  function detectOS() {
    var nav = window.navigator || {};
    var uaData = nav.userAgentData || {};
    var platform = (uaData.platform || '').toLowerCase();
    var ua = nav.userAgent || '';
    var hinted = null;

    /* 手机/平板运行不了 Seanbot。注意 iPad 的 UA 里带着 "like Mac OS X"，
       必须先拦移动端，否则会被当成 macOS。 */
    state.mobile = /Android|iPhone|iPad|iPod|Mobile/i.test(ua);
    if (state.mobile) return null;

    if (platform.indexOf('mac') === 0) hinted = 'macos';
    else if (platform.indexOf('windows') >= 0) hinted = 'windows';
    else if (platform.indexOf('linux') >= 0 || platform.indexOf('chrome os') >= 0) hinted = 'linux';

    if (!hinted) {
      if (/Windows NT/i.test(ua)) hinted = 'windows';
      else if (/Macintosh|Mac OS X/i.test(ua)) hinted = 'macos';
      else if (/Linux|X11|CrOS/i.test(ua)) hinted = 'linux';
    }

    return hinted;
  }

  /* Chromium 系浏览器可以问出 CPU 架构；Safari/Firefox 拿不到，就用默认值 */
  function detectArch() {
    var uaData = window.navigator && window.navigator.userAgentData;
    if (!uaData || typeof uaData.getHighEntropyValues !== 'function') return Promise.resolve(null);
    return uaData.getHighEntropyValues(['architecture', 'bitness']).then(function (values) {
      var arch = String(values.architecture || '').toLowerCase();
      if (arch === 'arm') return 'arm64';
      if (arch === 'x86') return String(values.bitness) === '64' ? 'x64' : null;
      return null;
    }).catch(function () { return null; });
  }

  /* ------------------------------------------------------------------ 渲染 */
  function product() { return PRODUCTS[state.product] || PRODUCTS.cli; }
  function platform() { return TARGETS[state.os] || TARGETS.macos; }

  function currentArch() {
    var list = platform().archs;
    for (var i = 0; i < list.length; i++) {
      if (list[i].id === state.arch) return list[i];
    }
    return list[0];
  }

  function renderStatus() {
    var p = product();
    els.statusPill.textContent = p.pill;
    els.statusPill.className = 'pill' + (p.pillClass ? ' ' + p.pillClass : '');
    els.statusText.textContent = p.statusText;
    els.downloadBtn.hidden = !p.share;
    els.wip.hidden = !!p.share;
    els.osRow.hidden = !p.share;
    els.archRow.hidden = !p.share || platform().archs.length < 2;
    els.installBox.hidden = !p.share;
  }

  function renderOS() {
    var buttons = els.osChips.querySelectorAll('.oschip');
    for (var i = 0; i < buttons.length; i++) {
      var active = buttons[i].getAttribute('data-os') === state.os;
      buttons[i].classList.toggle('is-active', active);
      buttons[i].setAttribute('aria-pressed', active ? 'true' : 'false');
    }

    /* 架构按钮按平台重建（Windows 只有 x64，就整行隐藏） */
    els.archChips.innerHTML = '';
    var archs = platform().archs;
    archs.forEach(function (arch) {
      var button = document.createElement('button');
      button.type = 'button';
      button.className = 'archchip' + (arch.id === state.arch ? ' is-active' : '');
      button.setAttribute('data-arch', arch.id);
      button.setAttribute('aria-pressed', arch.id === state.arch ? 'true' : 'false');
      button.textContent = arch.label;
      button.addEventListener('click', function () {
        state.arch = arch.id;
        render();
      });
      els.archChips.appendChild(button);
    });

    var note;
    if (state.detected) {
      note = '已识别：' + platform().detectNote + ' · ' + currentArch().label;
      if (state.mobile) note += '（移动端浏览器无法运行 Seanbot，请在电脑上下载）';
    } else if (state.mobile) {
      note = '移动端无法运行 Seanbot，请在电脑上打开本页（下面按 ' + platform().label + ' 预选）';
    } else {
      note = '没认出你的系统，先按 ' + platform().label + ' 显示，手动点一下更准';
    }
    els.detectNote.textContent = note;
  }

  function renderDownload() {
    var p = product();
    var arch = currentArch();
    var file = 'sean-' + arch.target + '.' + arch.ext;
    els.downloadLabel.textContent = '下载 ' + platform().label + '（' + arch.label + '）';
    els.downloadBtn.setAttribute('href', LATEST_DOWNLOAD + file);
    els.downloadMeta.textContent = file + ' · 校验和见 Release 里的 SHA256SUMS';
  }

  function renderInstall() {
    var p = product();
    var cmds = {
      cli: platform().install,
      tui: platform().install,
      app: ''
    };
    var notes = {
      cli: platform().installNote,
      tui: platform().installNote + ' TUI 与 CLI 共用同一个 sean 可执行文件。',
      app: ''
    };
    els.installTitle.textContent = state.os === 'windows' ? '一行命令安装（PowerShell）' : '一行命令安装';
    els.installCmd.textContent = cmds[state.product] || '';
    els.installNote.textContent = notes[state.product] || '';
    els.copyBtn.classList.remove('is-done');
    els.copyLabel.textContent = '复制';
  }

  /* 预览图：先探一次是否真的存在，存在才显示；否则保留占位说明 */
  var previewToken = 0;
  function renderPreview() {
    var p = product();
    els.previewTitle.textContent = p.windowTitle;
    els.previewPill.textContent = p.pill;
    els.previewPill.className = 'pill' + (p.pillClass ? ' ' + p.pillClass : '');
    els.previewCaption.textContent = p.caption;
    els.placeholderTitle.textContent = p.share ? '预览图正在补充' : '界面还在开发';
    els.placeholderNote.textContent = p.share
      ? '截图稍后补上；上面的下载与安装命令现在就能用。'
      : '桌面端尚未发布，先关注 GitHub Releases。';

    previewToken += 1;
    var token = previewToken;
    els.previewImg.hidden = true;
    els.previewImg.removeAttribute('src');
    els.previewImg.alt = p.name + ' 界面预览';
    els.placeholder.hidden = false;
    els.previewBody.classList.remove('is-media');

    var probe = new Image();
    probe.onload = function () {
      if (token !== previewToken) return;
      els.previewImg.src = p.preview;
      els.previewImg.hidden = false;
      els.placeholder.hidden = true;
      els.previewBody.classList.add('is-media');
    };
    probe.onerror = function () { /* 素材还没到位：保持占位 */ };
    probe.src = p.preview;
  }

  function renderTabs() {
    var tabs = els.tabs.querySelectorAll('.tab');
    for (var i = 0; i < tabs.length; i++) {
      var active = tabs[i].getAttribute('data-product') === state.product;
      tabs[i].classList.toggle('is-active', active);
      tabs[i].setAttribute('aria-selected', active ? 'true' : 'false');
      tabs[i].tabIndex = active ? 0 : -1;
    }
    var panel = $('panel-download');
    if (panel) panel.setAttribute('aria-labelledby', 'tab-' + state.product);
  }

  function render() {
    renderTabs();
    renderStatus();
    renderOS();
    renderDownload();
    renderInstall();
    renderPreview();
  }

  /* ------------------------------------------------------------ 事件绑定 */
  function bindTabs() {
    var tabs = Array.prototype.slice.call(els.tabs.querySelectorAll('.tab'));
    tabs.forEach(function (tab) {
      tab.addEventListener('click', function () {
        state.product = tab.getAttribute('data-product');
        render();
      });
      tab.addEventListener('keydown', function (event) {
        var index = tabs.indexOf(tab);
        var next = null;
        if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
        else if (event.key === 'ArrowLeft') next = (index - 1 + tabs.length) % tabs.length;
        else if (event.key === 'Home') next = 0;
        else if (event.key === 'End') next = tabs.length - 1;
        if (next === null) return;
        event.preventDefault();
        tabs[next].focus();
        state.product = tabs[next].getAttribute('data-product');
        render();
      });
    });
  }

  function bindOS() {
    var buttons = els.osChips.querySelectorAll('.oschip');
    for (var i = 0; i < buttons.length; i++) {
      buttons[i].addEventListener('click', function (event) {
        state.os = event.currentTarget.getAttribute('data-os');
        state.arch = TARGETS[state.os].defaultArch;
        state.detected = true;
        render();
      });
    }
  }

  function bindCopy() {
    els.copyBtn.addEventListener('click', function () {
      var text = els.installCmd.textContent || '';
      copyText(text).then(function (ok) {
        if (!ok) return;
        els.copyBtn.classList.add('is-done');
        els.copyLabel.textContent = '已复制';
        window.setTimeout(function () {
          els.copyBtn.classList.remove('is-done');
          els.copyLabel.textContent = '复制';
        }, 1600);
      });
    });
  }

  /* 剪贴板 API 在 http 下不可用，退回 textarea + execCommand */
  function copyText(text) {
    if (window.navigator.clipboard && window.isSecureContext) {
      return window.navigator.clipboard.writeText(text).then(function () { return true; }, function () { return legacyCopy(text); });
    }
    return Promise.resolve(legacyCopy(text));
  }

  function legacyCopy(text) {
    try {
      var area = document.createElement('textarea');
      area.value = text;
      area.setAttribute('readonly', '');
      area.style.position = 'fixed';
      area.style.opacity = '0';
      document.body.appendChild(area);
      area.select();
      var ok = document.execCommand('copy');
      document.body.removeChild(area);
      return ok;
    } catch (error) {
      return false;
    }
  }

  /* ---------------------------------------------------------------- 启动 */
  function init() {
    els.tabs = document.querySelector('.tabs');
    els.osChips = document.querySelector('.os__chips');
    els.archChips = $('arch-chips');
    els.archRow = $('arch-row');
    els.osRow = document.querySelector('.os');
    els.statusPill = $('status-pill');
    els.statusText = $('status-text');
    els.downloadBtn = $('download-btn');
    els.downloadLabel = $('download-label');
    els.downloadMeta = $('download-meta');
    els.installBox = $('install-box');
    els.installTitle = $('install-title');
    els.installCmd = $('install-cmd');
    els.installNote = $('install-note');
    els.copyBtn = $('copy-btn');
    els.copyLabel = $('copy-label');
    els.wip = $('wip-note');
    els.detectNote = $('detect-note');
    els.previewBody = document.querySelector('.window__body');
    els.previewImg = $('preview-img');
    els.previewTitle = $('preview-title');
    els.previewPill = $('preview-pill');
    els.previewCaption = $('preview-caption');
    els.placeholder = $('placeholder');
    els.placeholderTitle = $('placeholder-title');
    els.placeholderNote = $('placeholder-note');

    var detected = detectOS();
    state.detected = !!detected;
    state.os = detected || 'macos';
    state.arch = TARGETS[state.os].defaultArch;

    bindTabs();
    bindOS();
    bindCopy();
    render();

    /* 架构是异步问出来的，拿到后只更新与架构有关的部分 */
    detectArch().then(function (arch) {
      if (!arch) return;
      var list = platform().archs;
      var supported = list.some(function (item) { return item.id === arch; });
      if (!supported) return;
      state.arch = arch;
      renderOS();
      renderDownload();
    });
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
