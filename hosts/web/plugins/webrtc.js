// Opt-in TinyJump Web WebRTC <Native name="VideoCall" /> component (plain ES2017, zero deps).
// Uses native browser RTCPeerConnection + getUserMedia and signals over an existing @server
// object via `signals` prop and `onSignal` / `onState` / `onError` events.

const DEFAULT_ICE_SERVERS = [{ urls: 'stun:stun.l.google.com:19302' }];
const MAX_SDP_BYTES = 65536;
const MAX_ICE_BYTES = 8192;
const MAX_PENDING_ICE = 64;
const MAX_ICE_URL_LEN = 256;

function normId(v) {
  return typeof v === 'string' ? v.trim() : '';
}

function isValidIceUrl(u) {
  if (typeof u !== 'string') return false;
  const s = u.trim();
  if (s.length === 0 || s.length > MAX_ICE_URL_LEN) return false;
  const low = s.toLowerCase();
  let rest = '';
  if (low.indexOf('stuns:') === 0) rest = s.slice(6);
  else if (low.indexOf('stun:') === 0) rest = s.slice(5);
  else if (low.indexOf('turns:') === 0) rest = s.slice(6);
  else if (low.indexOf('turn:') === 0) rest = s.slice(5);
  else return false;
  if (rest.indexOf('//') === 0) rest = rest.slice(2);
  return rest.length > 0 && !/\s/.test(rest);
}

function sanitizeIceServers(raw) {
  if (!Array.isArray(raw) || raw.length === 0) return DEFAULT_ICE_SERVERS;
  const out = [];
  for (let i = 0; i < raw.length && out.length < 8; i++) {
    const entry = raw[i];
    if (!entry || typeof entry !== 'object') continue;
    let urls = [];
    if (typeof entry.urls === 'string' && isValidIceUrl(entry.urls)) {
      urls = [entry.urls.trim()];
    } else if (Array.isArray(entry.urls)) {
      for (let j = 0; j < entry.urls.length && urls.length < 8; j++) {
        if (isValidIceUrl(entry.urls[j])) urls.push(entry.urls[j].trim());
      }
    }
    if (urls.length === 0) continue;
    const item = { urls: urls };
    if (typeof entry.username === 'string' && entry.username.length <= 256) {
      item.username = entry.username;
    }
    if (typeof entry.credential === 'string' && entry.credential.length <= 256) {
      item.credential = entry.credential;
    }
    out.push(item);
  }
  return out.length > 0 ? out : DEFAULT_ICE_SERVERS;
}

function serializeIceKey(servers) {
  try {
    return JSON.stringify(servers);
  } catch (e) {
    return '';
  }
}

function serializeDesc(d) {
  return JSON.stringify({ type: d.type, sdp: d.sdp });
}

function serializeCandidate(c) {
  return JSON.stringify({
    candidate: c.candidate,
    sdpMid: c.sdpMid === undefined ? null : c.sdpMid,
    sdpMLineIndex: c.sdpMLineIndex === undefined ? null : c.sdpMLineIndex,
  });
}

function stopStream(stream) {
  if (!stream || typeof stream.getTracks !== 'function') return;
  const tracks = stream.getTracks();
  for (let i = 0; i < tracks.length; i++) {
    try { tracks[i].stop(); } catch (e) { /* ignore */ }
  }
}

function applyTrackToggles(stream, muted, cameraOff) {
  if (!stream || typeof stream.getTracks !== 'function') return;
  const audio = typeof stream.getAudioTracks === 'function' ? stream.getAudioTracks() : [];
  for (let i = 0; i < audio.length; i++) audio[i].enabled = !muted;
  const video = typeof stream.getVideoTracks === 'function' ? stream.getVideoTracks() : [];
  for (let i = 0; i < video.length; i++) video[i].enabled = !cameraOff;
}

function maxSignalSeq(signals) {
  if (!Array.isArray(signals)) return 0;
  let hi = 0;
  for (let i = 0; i < signals.length; i++) {
    const s = signals[i];
    if (s && typeof s.seq === 'number' && s.seq > hi) hi = s.seq;
  }
  return hi;
}

function describeFallback(prefix, err) {
  const name = err && typeof err.name === 'string' ? err.name : '';
  if (name === 'NotReadableError' || name === 'TrackStartError') {
    return prefix + ' (in use by another browser/app)';
  }
  if (name === 'NotFoundError' || name === 'DevicesNotFoundError') {
    return prefix + ' (device not found)';
  }
  if (name === 'NotAllowedError' || name === 'PermissionDeniedError') {
    return prefix + ' (permission denied)';
  }
  return prefix;
}

function makeOverlayButton(text, bg) {
  const b = document.createElement('button');
  b.type = 'button';
  b.textContent = text;
  b.style.border = '1px solid rgba(255,255,255,0.22)';
  b.style.borderRadius = '999px';
  b.style.padding = '5px 11px';
  b.style.fontSize = '12px';
  b.style.fontWeight = '600';
  b.style.lineHeight = '1.2';
  b.style.cursor = 'pointer';
  b.style.color = '#f9fafb';
  b.style.background = bg || 'rgba(17,24,39,0.78)';
  return b;
}

/**
 * Creates the `<Native name="VideoCall" ... />` host component spec for `TinyJump`.
 */
export function createVideoCallComponent() {
  return {
    create: function (initialProps, rawEmit) {
      const root = document.createElement('div');
      root.className = 'tj-videocall';
      root.style.position = 'relative';
      root.style.overflow = 'hidden';
      root.style.background = '#111318';
      root.style.borderRadius = '12px';
      root.style.minHeight = '260px';
      root.style.width = '100%';
      root.style.display = 'flex';
      root.style.alignItems = 'center';
      root.style.justifyContent = 'center';
      root.style.color = '#e5e7eb';
      root.style.fontFamily = 'system-ui, -apple-system, sans-serif';

      const remoteVideo = document.createElement('video');
      remoteVideo.autoplay = true;
      remoteVideo.playsInline = true;
      remoteVideo.setAttribute('playsinline', '');
      remoteVideo.style.width = '100%';
      remoteVideo.style.height = '100%';
      remoteVideo.style.objectFit = 'contain';
      remoteVideo.style.display = 'none';

      const localVideo = document.createElement('video');
      localVideo.autoplay = true;
      localVideo.muted = true;
      localVideo.playsInline = true;
      localVideo.setAttribute('playsinline', '');
      localVideo.style.position = 'absolute';
      localVideo.style.right = '12px';
      localVideo.style.bottom = '12px';
      localVideo.style.width = '120px';
      localVideo.style.height = '90px';
      localVideo.style.objectFit = 'cover';
      localVideo.style.borderRadius = '8px';
      localVideo.style.border = '1px solid rgba(255,255,255,0.28)';
      localVideo.style.background = '#000';
      localVideo.style.transform = 'scaleX(-1)';
      localVideo.style.zIndex = '2';
      localVideo.style.display = 'none';

      const badge = document.createElement('div');
      badge.style.position = 'absolute';
      badge.style.left = '12px';
      badge.style.top = '10px';
      badge.style.padding = '4px 10px';
      badge.style.borderRadius = '999px';
      badge.style.fontSize = '12px';
      badge.style.background = 'rgba(0,0,0,0.62)';
      badge.style.color = '#f3f4f6';
      badge.style.zIndex = '3';
      badge.textContent = 'Idle';

      const noticePill = document.createElement('div');
      noticePill.style.position = 'absolute';
      noticePill.style.left = '12px';
      noticePill.style.top = '40px';
      noticePill.style.maxWidth = 'calc(100% - 24px)';
      noticePill.style.padding = '5px 10px';
      noticePill.style.borderRadius = '8px';
      noticePill.style.fontSize = '12px';
      noticePill.style.background = 'rgba(120,53,15,0.88)';
      noticePill.style.border = '1px solid rgba(251,191,36,0.45)';
      noticePill.style.color = '#fef3c7';
      noticePill.style.zIndex = '3';
      noticePill.style.display = 'none';

      const placeholder = document.createElement('div');
      placeholder.style.fontSize = '14px';
      placeholder.style.color = '#9ca3af';
      placeholder.style.textAlign = 'center';
      placeholder.style.padding = '16px';
      placeholder.style.zIndex = '1';
      placeholder.textContent = 'Ready to start call';

      const controlsBar = document.createElement('div');
      controlsBar.style.position = 'absolute';
      controlsBar.style.left = '12px';
      controlsBar.style.bottom = '10px';
      controlsBar.style.display = 'none';
      controlsBar.style.gap = '6px';
      controlsBar.style.flexWrap = 'wrap';
      controlsBar.style.alignItems = 'center';
      controlsBar.style.zIndex = '3';

      const shareBtn = makeOverlayButton('Share Screen', 'rgba(37,99,235,0.88)');
      const retryBtn = makeOverlayButton('Retry Cam/Mic', 'rgba(180,83,9,0.9)');
      retryBtn.style.display = 'none';
      const unmuteBtn = makeOverlayButton('Unmute Audio', 'rgba(15,118,110,0.92)');
      unmuteBtn.style.display = 'none';

      controlsBar.appendChild(shareBtn);
      controlsBar.appendChild(retryBtn);
      controlsBar.appendChild(unmuteBtn);

      root.appendChild(remoteVideo);
      root.appendChild(placeholder);
      root.appendChild(localVideo);
      root.appendChild(badge);
      root.appendChild(noticePill);
      root.appendChild(controlsBar);

      const st = {
        props: Object.assign({}, initialProps || {}),
        destroyed: false,
        mediaGen: 0,
        peerGen: 0,
        screenGen: 0,
        retryGen: 0,
        retrying: false,
        callState: 'idle',
        baseBadge: 'Idle',
        mediaKey: '',
        mediaReady: false,
        mediaMode: 'idle',
        mediaNotice: '',
        compact: false,
        autoplayMuted: false,
        lastPropScreenShare: false,
        peerKey: '',
        endedPeerKey: '',
        localStream: null,
        screenStream: null,
        screenActive: false,
        screenWanted: false,
        screenSenderAdded: null,
        screenUpgradedTransceiver: null,
        screenPrevDirection: '',
        needsRenegotiate: false,
        remoteStream: null,
        pc: null,
        lastSeq: 0,
        pendingIce: [],
        offerSentFor: '',
        iceServers: sanitizeIceServers(initialProps && initialProps.iceServers),
        signalChain: Promise.resolve(),
      };

      // Always deliver events on a microtask so tinyjump.js has finished setting el.__tj
      // during create() and clearing el.__quiet = false after update().
      function emit(event, value) {
        Promise.resolve().then(function () {
          if (!st.destroyed) rawEmit(event, value);
        });
      }

      function getLiveTracks(stream, kind) {
        if (!stream) return [];
        const fn = kind === 'audio' ? 'getAudioTracks' : 'getVideoTracks';
        if (typeof stream[fn] !== 'function') return [];
        const list = stream[fn]();
        const live = [];
        for (let i = 0; i < list.length; i++) {
          if (list[i] && list[i].readyState !== 'ended') live.push(list[i]);
        }
        return live;
      }

      function canDegrade() {
        const wantA = st.props.audio !== false;
        const wantV = st.props.video !== false;
        const hasA = getLiveTracks(st.localStream, 'audio').length > 0;
        const hasV = getLiveTracks(st.localStream, 'video').length > 0;
        return (wantA && !hasA) || (wantV && !hasV);
      }

      function renderOverlayControls() {
        const showBar = Boolean(st.props.active) && st.props.controls !== false && !st.compact;
        controlsBar.style.display = showBar ? 'flex' : 'none';
        shareBtn.textContent = st.screenActive || st.screenWanted ? 'Stop Share' : 'Share Screen';
        shareBtn.style.background = st.screenActive ? 'rgba(185,28,28,0.9)' : 'rgba(37,99,235,0.88)';
        retryBtn.style.display = showBar && st.mediaReady && canDegrade() ? 'inline-block' : 'none';
        unmuteBtn.style.display = st.autoplayMuted && showBar ? 'inline-block' : 'none';
        if (!st.mediaNotice || st.compact) {
          noticePill.style.display = 'none';
          noticePill.textContent = '';
        } else {
          noticePill.textContent = st.mediaNotice;
          noticePill.style.display = 'block';
        }
      }

      function setMediaState(mode, notice) {
        if (st.destroyed) return;
        st.mediaMode = mode || 'idle';
        st.mediaNotice = notice || '';
        renderOverlayControls();
        renderBadge();
        emit('onMediaMode', st.mediaMode);
        emit('onNotice', st.mediaNotice);
      }

      function setNotice(msg) {
        if (st.destroyed) return;
        st.mediaNotice = msg || '';
        renderOverlayControls();
        emit('onNotice', st.mediaNotice);
      }

      function applyHeight(props) {
        const wasCompact = st.compact;
        const h = props && typeof props.height === 'number' && props.height > 0 ? Math.max(48, Math.round(props.height)) : 260;
        st.compact = h < 140;
        root.style.minHeight = h + 'px';
        root.style.height = h < 200 ? h + 'px' : '';
        if (st.compact) {
          try { localVideo.srcObject = null; } catch (e) { /* ignore */ }
          localVideo.style.display = 'none';
        } else if (wasCompact && st.mediaReady) {
          applyLocalPreview();
        }
        renderOverlayControls();
      }

      function renderBadge() {
        let label = st.baseBadge;
        if (st.mediaMode === 'recv-only' && st.props.active) label += ' · View-only';
        else if (st.mediaMode === 'audio-only' && st.props.active && st.props.video !== false) label += ' · Audio-only';
        else if (st.mediaMode === 'video-only' && st.props.active && st.props.audio !== false) label += ' · Video-only';
        if (st.screenActive) label += ' · Sharing';
        badge.textContent = label;
      }

      applyHeight(st.props);

      function setCallState(next, label) {
        if (st.destroyed) return;
        st.baseBadge = label || next;
        renderBadge();
        if (st.callState !== next) {
          st.callState = next;
          emit('onState', next);
        }
      }

      function failCall(msg) {
        if (st.destroyed) return;
        st.baseBadge = 'Error';
        renderBadge();
        placeholder.textContent = msg;
        placeholder.style.display = 'block';
        emit('onError', msg);
        setCallState('failed', 'Error');
      }

      function findTransceiverByKind(pc, kind) {
        if (!pc || typeof pc.getTransceivers !== 'function') return null;
        const trs = pc.getTransceivers();
        for (let i = 0; i < trs.length; i++) {
          const tr = trs[i];
          if (!tr) continue;
          const sKind = tr.sender && tr.sender.track && tr.sender.track.kind;
          const rKind = tr.receiver && tr.receiver.track && tr.receiver.track.kind;
          if (sKind === kind || rKind === kind || tr._tjKind === kind) return tr;
        }
        return null;
      }

      function findVideoTransceiver(pc) {
        return findTransceiverByKind(pc, 'video');
      }

      function findVideoSender(pc) {
        if (!pc) return null;
        if (typeof pc.getSenders === 'function') {
          const senders = pc.getSenders();
          for (let i = 0; i < senders.length; i++) {
            const s = senders[i];
            if (s && s.track && s.track.kind === 'video') return s;
          }
        }
        const tr = findVideoTransceiver(pc);
        if (tr && tr.sender) return tr.sender;
        return st.screenSenderAdded;
      }

      function ensureReceiveTransceivers(pc) {
        if (!pc || typeof pc.addTransceiver !== 'function') return;
        const trs = typeof pc.getTransceivers === 'function' ? pc.getTransceivers() : [];
        let hasAudio = false;
        let hasVideo = false;
        for (let i = 0; i < trs.length; i++) {
          const tr = trs[i];
          if (!tr) continue;
          const sk = tr.sender && tr.sender.track && tr.sender.track.kind;
          const rk = tr.receiver && tr.receiver.track && tr.receiver.track.kind;
          const tk = tr._tjKind;
          if (sk === 'audio' || rk === 'audio' || tk === 'audio') hasAudio = true;
          if (sk === 'video' || rk === 'video' || tk === 'video') hasVideo = true;
        }
        if (!hasAudio) {
          try {
            const aTr = pc.addTransceiver('audio', { direction: 'recvonly' });
            if (aTr) aTr._tjKind = 'audio';
          } catch (e) { /* ignore */ }
        }
        if (!hasVideo) {
          try {
            const vTr = pc.addTransceiver('video', { direction: 'recvonly' });
            if (vTr) vTr._tjKind = 'video';
          } catch (e) { /* ignore */ }
        }
      }

      function playRemote() {
        if (!remoteVideo || typeof remoteVideo.play !== 'function') return;
        const p = remoteVideo.play();
        if (p && typeof p.catch === 'function') {
          p.catch(function () {
            if (st.destroyed) return;
            remoteVideo.muted = true;
            st.autoplayMuted = true;
            renderOverlayControls();
            const p2 = remoteVideo.play();
            if (p2 && typeof p2.catch === 'function') p2.catch(function () {});
          });
        }
      }

      function applyLocalPreview() {
        if (st.compact) {
          try { localVideo.srcObject = null; } catch (e) { /* ignore */ }
          localVideo.style.display = 'none';
          return;
        }
        if (st.screenStream) {
          localVideo.srcObject = st.screenStream;
          localVideo.style.transform = 'none';
          localVideo.style.display = 'block';
          return;
        }
        const wantVideo = st.props.video !== false;
        if (st.localStream && wantVideo && getLiveTracks(st.localStream, 'video').length > 0) {
          localVideo.srcObject = st.localStream;
          localVideo.style.transform = 'scaleX(-1)';
          localVideo.style.display = 'block';
        } else {
          try { localVideo.srcObject = null; } catch (e) { /* ignore */ }
          localVideo.style.display = 'none';
        }
      }

      function closePeerConnection() {
        st.peerGen += 1;
        st.pendingIce = [];
        st.offerSentFor = '';
        st.screenSenderAdded = null;
        st.screenUpgradedTransceiver = null;
        st.screenPrevDirection = '';
        st.needsRenegotiate = false;
        st.signalChain = Promise.resolve();
        if (st.pc) {
          try { st.pc.onicecandidate = null; } catch (e) { /* ignore */ }
          try { st.pc.ontrack = null; } catch (e) { /* ignore */ }
          try { st.pc.onconnectionstatechange = null; } catch (e) { /* ignore */ }
          try { st.pc.oniceconnectionstatechange = null; } catch (e) { /* ignore */ }
          try { st.pc.onsignalingstatechange = null; } catch (e) { /* ignore */ }
          try { st.pc.close(); } catch (e) { /* ignore */ }
          st.pc = null;
        }
        st.remoteStream = null;
        try { remoteVideo.srcObject = null; } catch (e) { /* ignore */ }
        remoteVideo.style.display = 'none';
        st.autoplayMuted = false;
        remoteVideo.muted = false;
        unmuteBtn.style.display = 'none';
      }

      function stopScreenShare(emitOff) {
        st.screenGen += 1;
        const wasActive = st.screenActive || Boolean(st.screenStream);
        st.screenActive = false;
        if (st.screenStream) {
          stopStream(st.screenStream);
          st.screenStream = null;
        }
        applyLocalPreview();
        const camTracks = getLiveTracks(st.localStream, 'video');
        const restoreTrack = camTracks.length > 0 ? camTracks[0] : null;
        if (st.pc) {
          if (!restoreTrack && st.screenUpgradedTransceiver) {
            const upTr = st.screenUpgradedTransceiver;
            const prevDir = st.screenPrevDirection || 'recvonly';
            st.screenUpgradedTransceiver = null;
            st.screenPrevDirection = '';
            if (upTr.sender && typeof upTr.sender.replaceTrack === 'function') {
              try {
                const p = upTr.sender.replaceTrack(null);
                if (p && typeof p.catch === 'function') p.catch(function () {});
              } catch (e) { /* ignore */ }
            }
            try { upTr.direction = prevDir; } catch (e) { /* ignore */ }
            if (st.props.active) renegotiate(st.peerGen);
          } else if (!restoreTrack && st.screenSenderAdded && st.props.active && typeof st.pc.removeTrack === 'function') {
            try { st.pc.removeTrack(st.screenSenderAdded); } catch (e) { /* ignore */ }
            st.screenSenderAdded = null;
            renegotiate(st.peerGen);
          } else {
            const sender = findVideoSender(st.pc);
            if (sender && typeof sender.replaceTrack === 'function') {
              try {
                const p = sender.replaceTrack(restoreTrack);
                if (p && typeof p.catch === 'function') p.catch(function () {});
              } catch (e) { /* ignore */ }
            }
          }
        }
        renderBadge();
        renderOverlayControls();
        if (emitOff && wasActive && !st.destroyed) emit('onScreenShare', false);
      }

      function teardownAll(emitIdle) {
        st.mediaGen += 1;
        st.retryGen += 1;
        st.retrying = false;
        retryBtn.disabled = false;
        retryBtn.textContent = 'Retry Cam/Mic';
        st.mediaKey = '';
        st.mediaReady = false;
        st.mediaMode = 'idle';
        st.mediaNotice = '';
        st.peerKey = '';
        st.endedPeerKey = '';
        st.screenWanted = false;
        st.lastPropScreenShare = false;
        closePeerConnection();
        stopScreenShare(false);
        if (st.localStream) {
          stopStream(st.localStream);
          st.localStream = null;
        }
        try { localVideo.srcObject = null; } catch (e) { /* ignore */ }
        localVideo.style.display = 'none';
        placeholder.style.display = 'block';
        placeholder.textContent = 'Ready to start call';
        renderOverlayControls();
        if (emitIdle && !st.destroyed) {
          setMediaState('idle', '');
          setCallState('idle', 'Idle');
        }
      }

      function createPeerConnection(pGen) {
        if (typeof window === 'undefined' || typeof window.RTCPeerConnection !== 'function') {
          failCall('WebRTC is not supported in this browser');
          return null;
        }
        const pc = new window.RTCPeerConnection({ iceServers: st.iceServers });
        st.remoteStream = typeof window.MediaStream === 'function' ? new window.MediaStream() : null;

        pc.onicecandidate = function (ev) {
          if (st.destroyed || st.peerGen !== pGen || !ev.candidate) return;
          const to = normId(st.props.remotePeerId);
          if (!to) return;
          const raw = serializeCandidate(ev.candidate);
          if (raw.length <= MAX_ICE_BYTES) {
            emit('onSignal', { to: to, kind: 'ice', payload: raw });
          }
        };

        pc.ontrack = function (ev) {
          if (st.destroyed || st.peerGen !== pGen || !ev.track) return;
          if (st.remoteStream && typeof st.remoteStream.addTrack === 'function') {
            const existing = typeof st.remoteStream.getTracks === 'function' ? st.remoteStream.getTracks() : [];
            let hasThis = false;
            for (let i = 0; i < existing.length; i++) {
              const tr = existing[i];
              if (tr === ev.track) {
                hasThis = true;
              } else if (tr.kind === ev.track.kind || tr.readyState === 'ended') {
                try { st.remoteStream.removeTrack(tr); } catch (e) { /* ignore */ }
              }
            }
            if (!hasThis) {
              try { st.remoteStream.addTrack(ev.track); } catch (e) { /* ignore */ }
            }
            remoteVideo.srcObject = st.remoteStream;
            ev.track.onunmute = function () {
              if (st.destroyed || st.peerGen !== pGen) return;
              remoteVideo.style.display = 'block';
              placeholder.style.display = 'none';
              playRemote();
            };
            ev.track.onended = function () {
              if (st.destroyed || st.peerGen !== pGen || !st.remoteStream) return;
              try { st.remoteStream.removeTrack(ev.track); } catch (e) { /* ignore */ }
              const hasAny = typeof st.remoteStream.getTracks === 'function' && st.remoteStream.getTracks().length > 0;
              if (!hasAny) {
                remoteVideo.style.display = 'none';
                placeholder.style.display = 'block';
              }
            };
          } else if (ev.streams && ev.streams[0]) {
            remoteVideo.srcObject = ev.streams[0];
          }
          remoteVideo.style.display = 'block';
          placeholder.style.display = 'none';
          playRemote();
        };

        function onStateChange() {
          if (st.destroyed || st.peerGen !== pGen) return;
          const cs = pc.connectionState || pc.iceConnectionState || '';
          if (cs === 'connected' || cs === 'completed') {
            setCallState('connected', 'Connected');
          } else if (cs === 'connecting' || cs === 'checking') {
            setCallState('connecting', 'Connecting...');
          } else if (cs === 'disconnected') {
            setCallState('disconnected', 'Reconnecting...');
          } else if (cs === 'failed') {
            setCallState('failed', 'Connection failed');
          } else if (cs === 'closed') {
            setCallState('closed', 'Ended');
          }
        }
        pc.onconnectionstatechange = onStateChange;
        pc.oniceconnectionstatechange = onStateChange;
        pc.onsignalingstatechange = function () {
          if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
          if (st.pc.signalingState === 'stable' && st.needsRenegotiate) {
            renegotiate(pGen);
          }
        };

        const screenTracks = st.screenStream && typeof st.screenStream.getVideoTracks === 'function'
          ? st.screenStream.getVideoTracks()
          : [];
        const screenVideo = screenTracks.length > 0 ? screenTracks[0] : null;
        let addedScreen = false;
        if (st.localStream) {
          const tracks = st.localStream.getTracks();
          for (let i = 0; i < tracks.length; i++) {
            if (tracks[i].kind === 'video' && screenVideo) {
              pc.addTrack(screenVideo, st.localStream);
              addedScreen = true;
            } else {
              pc.addTrack(tracks[i], st.localStream);
            }
          }
        }
        if (screenVideo && !addedScreen) {
          st.screenSenderAdded = pc.addTrack(screenVideo, st.localStream || st.screenStream);
        }
        return pc;
      }

      async function flushPendingIce(pc, pGen) {
        while (st.pendingIce.length > 0) {
          if (st.destroyed || st.peerGen !== pGen || st.pc !== pc) return;
          const cand = st.pendingIce.shift();
          try {
            await pc.addIceCandidate(cand);
          } catch (e) {
            /* ignore stale ICE candidate */
          }
        }
      }

      async function sendOffer(pGen, remote, label) {
        if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
        if (st.pc.signalingState !== 'stable') {
          st.needsRenegotiate = true;
          return;
        }
        st.needsRenegotiate = false;
        if (label) setCallState('connecting', label);
        try {
          ensureReceiveTransceivers(st.pc);
          const offer = await st.pc.createOffer({ offerToReceiveAudio: true, offerToReceiveVideo: true });
          if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
          await st.pc.setLocalDescription(offer);
          if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
          const raw = serializeDesc(st.pc.localDescription);
          if (raw.length > MAX_SDP_BYTES) {
            failCall('SDP offer exceeds size limit');
            return;
          }
          emit('onSignal', { to: remote, kind: 'offer', payload: raw });
        } catch (err) {
          if (st.destroyed || st.peerGen !== pGen) return;
          st.offerSentFor = '';
          failCall((err && err.message) || 'failed to create call offer');
        }
      }

      function maybeStartOffer(pGen) {
        if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
        const remote = normId(st.props.remotePeerId);
        if (!remote || !st.props.initiator) return;
        if (st.offerSentFor === remote) return;
        if (st.pc.signalingState !== 'stable') return;
        st.offerSentFor = remote;
        sendOffer(pGen, remote, 'Calling ' + remote + '...');
      }

      function renegotiate(pGen) {
        if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
        const remote = normId(st.props.remotePeerId);
        if (!remote) return;
        if (st.pc.signalingState !== 'stable') {
          st.needsRenegotiate = true;
          return;
        }
        st.needsRenegotiate = false;
        sendOffer(pGen, remote, null);
      }

      async function startScreenShare() {
        if (typeof navigator === 'undefined' || !navigator.mediaDevices || typeof navigator.mediaDevices.getDisplayMedia !== 'function') {
          st.screenWanted = false;
          renderOverlayControls();
          setNotice('Screen sharing is not supported in this browser (requires HTTPS or localhost)');
          emit('onScreenShare', false);
          return;
        }
        const sGen = ++st.screenGen;
        let stream;
        try {
          stream = await navigator.mediaDevices.getDisplayMedia({ video: true, audio: false });
        } catch (err) {
          if (st.destroyed || st.screenGen !== sGen) return;
          st.screenWanted = false;
          st.screenActive = false;
          renderOverlayControls();
          if (err && err.name === 'NotAllowedError') {
            setNotice('Screen share cancelled or blocked — click Share Screen to try again');
          } else if (err && err.name !== 'AbortError') {
            setNotice((err && err.message) || 'Could not start screen share');
          }
          emit('onScreenShare', false);
          return;
        }
        if (st.destroyed || st.screenGen !== sGen || !st.props.active || !st.screenWanted) {
          stopStream(stream);
          return;
        }
        const tracks = typeof stream.getVideoTracks === 'function' ? stream.getVideoTracks() : [];
        if (tracks.length === 0) {
          stopStream(stream);
          st.screenWanted = false;
          renderOverlayControls();
          emit('onScreenShare', false);
          return;
        }
        const screenTrack = tracks[0];
        st.screenStream = stream;
        st.screenActive = true;
        st.screenUpgradedTransceiver = null;
        st.screenPrevDirection = '';
        screenTrack.onended = function () {
          if (!st.destroyed && st.screenGen === sGen) {
            st.screenWanted = false;
            stopScreenShare(true);
          }
        };
        applyLocalPreview();
        if (st.pc) {
          const tr = findVideoTransceiver(st.pc);
          if (tr && tr.sender && typeof tr.sender.replaceTrack === 'function') {
            const prevDir = tr.direction || 'recvonly';
            if (prevDir !== 'sendrecv' && prevDir !== 'sendonly') {
              st.screenUpgradedTransceiver = tr;
              st.screenPrevDirection = prevDir;
              try { tr.direction = prevDir === 'recvonly' ? 'sendrecv' : 'sendonly'; } catch (e) { /* ignore */ }
              try {
                const p = tr.sender.replaceTrack(screenTrack);
                if (p && typeof p.catch === 'function') p.catch(function () {});
              } catch (e) { /* ignore */ }
              renegotiate(st.peerGen);
            } else {
              try {
                const p = tr.sender.replaceTrack(screenTrack);
                if (p && typeof p.catch === 'function') p.catch(function () {});
              } catch (e) { /* ignore */ }
            }
          } else {
            const sender = findVideoSender(st.pc);
            if (sender && typeof sender.replaceTrack === 'function') {
              try {
                const p = sender.replaceTrack(screenTrack);
                if (p && typeof p.catch === 'function') p.catch(function () {});
              } catch (e) { /* ignore */ }
            } else {
              st.screenSenderAdded = st.pc.addTrack(screenTrack, st.localStream || stream);
              renegotiate(st.peerGen);
            }
          }
        }
        renderBadge();
        renderOverlayControls();
        emit('onScreenShare', true);
      }

      function syncScreenShare(wantScreen) {
        if (wantScreen && !st.screenWanted) {
          st.screenWanted = true;
          renderOverlayControls();
          startScreenShare();
        } else if (!wantScreen && st.screenWanted) {
          st.screenWanted = false;
          stopScreenShare(false);
        }
      }

      function enqueueSignals(signals, pGen) {
        if (!Array.isArray(signals) || signals.length === 0) return;
        const me = normId(st.props.peerId);
        const remote = normId(st.props.remotePeerId);
        if (!me || !remote) return;

        // If the room reset its signals to a lower seq range, reset lastSeq cleanly.
        const hi = maxSignalSeq(signals);
        if (hi < st.lastSeq) st.lastSeq = 0;

        const todo = [];
        for (let i = 0; i < signals.length; i++) {
          const sig = signals[i];
          if (!sig || typeof sig.seq !== 'number' || sig.seq <= st.lastSeq) continue;
          if (normId(sig.to) !== me || normId(sig.from) !== remote) continue;
          todo.push(sig);
        }
        if (todo.length === 0) return;
        todo.sort(function (a, b) { return a.seq - b.seq; });

        st.signalChain = st.signalChain
          .then(async function () {
            for (let i = 0; i < todo.length; i++) {
              if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
              const sig = todo[i];
              if (sig.seq <= st.lastSeq) continue;
              st.lastSeq = sig.seq;
              const kind = sig.kind;
              const payload = typeof sig.payload === 'string' ? sig.payload : '';
              if (kind === 'bye') {
                // Close peer connection only; keep localStream alive while `active` remains true,
                // and mark this peerKey ended so subsequent re-renders don't recreate a PC until remotePeerId changes.
                st.endedPeerKey = st.peerKey;
                closePeerConnection();
                placeholder.style.display = 'block';
                placeholder.textContent = 'Peer left the call. Waiting for another participant...';
                setCallState('disconnected', 'Peer left');
                return;
              }
              if (kind === 'offer') {
                if (payload.length > MAX_SDP_BYTES) continue;
                let desc;
                try { desc = JSON.parse(payload); } catch (e) { continue; }
                if (!desc || desc.type !== 'offer' || typeof desc.sdp !== 'string') continue;
                try {
                  let resumeRenegotiate = st.needsRenegotiate;
                  if (st.pc.signalingState !== 'stable') {
                    if (st.props.initiator) continue; // impolite peer ignores colliding offer
                    resumeRenegotiate = true;
                    st.needsRenegotiate = false;
                    await st.pc.setLocalDescription({ type: 'rollback' });
                    if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
                  }
                  setCallState('connecting', 'Answering...');
                  await st.pc.setRemoteDescription(desc);
                  if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
                  await flushPendingIce(st.pc, pGen);
                  const answer = await st.pc.createAnswer();
                  if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
                  if (st.needsRenegotiate) {
                    resumeRenegotiate = true;
                    st.needsRenegotiate = false;
                  }
                  await st.pc.setLocalDescription(answer);
                  if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
                  const raw = serializeDesc(st.pc.localDescription);
                  if (raw.length > MAX_SDP_BYTES) {
                    failCall('SDP answer exceeds size limit');
                    return;
                  }
                  emit('onSignal', { to: remote, kind: 'answer', payload: raw });
                  if (resumeRenegotiate) st.needsRenegotiate = true;
                  if (st.needsRenegotiate && st.pc.signalingState === 'stable') {
                    renegotiate(pGen);
                  }
                } catch (err) {
                  if (!st.destroyed && st.peerGen === pGen) {
                    failCall((err && err.message) || 'failed to accept call offer');
                  }
                }
              } else if (kind === 'answer') {
                if (payload.length > MAX_SDP_BYTES) continue;
                let desc;
                try { desc = JSON.parse(payload); } catch (e) { continue; }
                if (!desc || desc.type !== 'answer' || typeof desc.sdp !== 'string') continue;
                try {
                  if (st.pc.signalingState === 'have-local-offer') {
                    await st.pc.setRemoteDescription(desc);
                    if (st.destroyed || st.peerGen !== pGen || !st.pc) return;
                    await flushPendingIce(st.pc, pGen);
                    if (st.needsRenegotiate && st.pc.signalingState === 'stable') {
                      renegotiate(pGen);
                    }
                  }
                } catch (err) {
                  if (!st.destroyed && st.peerGen === pGen) {
                    failCall((err && err.message) || 'failed to apply call answer');
                  }
                }
              } else if (kind === 'ice') {
                if (payload.length > MAX_ICE_BYTES) continue;
                let cand;
                try { cand = JSON.parse(payload); } catch (e) { continue; }
                if (!cand || typeof cand !== 'object' || Array.isArray(cand) || typeof cand.candidate !== 'string') {
                  continue;
                }
                if (st.pc.remoteDescription && st.pc.remoteDescription.type) {
                  try { await st.pc.addIceCandidate(cand); } catch (e) { /* ignore */ }
                } else if (st.pendingIce.length < MAX_PENDING_ICE) {
                  st.pendingIce.push(cand);
                }
              }
            }
          })
          .catch(function () {});
      }

      function ensurePeerConnection() {
        if (st.destroyed || !st.mediaReady) return;
        const me = normId(st.props.peerId);
        const remote = normId(st.props.remotePeerId);
        const iceKey = serializeIceKey(st.iceServers);
        const nextPeerKey = me + '|' + remote + '|' + iceKey;

        if (!remote) {
          if (st.pc || st.peerKey !== nextPeerKey) {
            closePeerConnection();
            st.peerKey = nextPeerKey;
            st.endedPeerKey = '';
          }
          placeholder.style.display = 'block';
          placeholder.textContent = 'Waiting for another participant...';
          setCallState('connecting', 'Waiting...');
          return;
        }

        if (st.peerKey === nextPeerKey && st.endedPeerKey === nextPeerKey) {
          return;
        }

        if (!st.pc || st.peerKey !== nextPeerKey) {
          closePeerConnection();
          st.peerKey = nextPeerKey;
          st.endedPeerKey = '';
          st.lastSeq = 0;
          const pGen = st.peerGen;
          const pc = createPeerConnection(pGen);
          if (!pc) return;
          st.pc = pc;
          placeholder.style.display = 'block';
          placeholder.textContent = 'Connecting to ' + remote + '...';
          setCallState('connecting', 'Connecting...');
          enqueueSignals(st.props.signals, pGen);
          maybeStartOffer(pGen);
          return;
        }

        enqueueSignals(st.props.signals, st.peerGen);
        maybeStartOffer(st.peerGen);
      }

      async function tryCaptureWithFallback(wantAudio, wantVideo) {
        if (typeof navigator === 'undefined' || !navigator.mediaDevices || typeof navigator.mediaDevices.getUserMedia !== 'function') {
          return {
            stream: null,
            mode: 'recv-only',
            notice: 'Camera & mic unavailable (requires HTTPS or localhost) — view-only mode (can still watch & share screen)',
          };
        }
        let firstErr = null;
        try {
          const s1 = await navigator.mediaDevices.getUserMedia({ audio: wantAudio, video: wantVideo });
          const mode1 = wantAudio && wantVideo ? 'audio-video' : wantAudio ? 'audio-only' : 'video-only';
          return { stream: s1, mode: mode1, notice: '' };
        } catch (err) {
          firstErr = err;
        }
        if (wantAudio && wantVideo) {
          try {
            const sAudio = await navigator.mediaDevices.getUserMedia({ audio: true, video: false });
            return {
              stream: sAudio,
              mode: 'audio-only',
              notice: describeFallback('Camera unavailable', firstErr) + ' — joined audio-only (can still view video & share screen)',
            };
          } catch (e2) { /* fall through to video-only */ }
          try {
            const sVideo = await navigator.mediaDevices.getUserMedia({ audio: false, video: true });
            return {
              stream: sVideo,
              mode: 'video-only',
              notice: describeFallback('Microphone unavailable', firstErr) + ' — joined video-only (can still listen & share screen)',
            };
          } catch (e3) { /* fall through to recv-only */ }
          return {
            stream: null,
            mode: 'recv-only',
            notice: describeFallback('Camera & mic unavailable', firstErr) + ' — view-only mode (can still watch & share screen)',
          };
        }
        const label = wantAudio ? 'Microphone unavailable' : 'Camera unavailable';
        const suffix = wantAudio ? ' — view-only mode (can still listen & share screen)' : ' — view-only mode (can still watch & share screen)';
        return {
          stream: null,
          mode: 'recv-only',
          notice: describeFallback(label, firstErr) + suffix,
        };
      }

      async function acquireMedia(mGen, wantAudio, wantVideo) {
        if (!wantAudio && !wantVideo) {
          if (typeof window === 'undefined' || typeof window.RTCPeerConnection !== 'function') {
            failCall('WebRTC is not supported in this browser');
            return;
          }
          st.localStream = null;
          st.mediaReady = true;
          setMediaState('recv-only', '');
          applyLocalPreview();
          ensurePeerConnection();
          st.lastPropScreenShare = Boolean(st.props.screenShare);
          if (st.lastPropScreenShare) syncScreenShare(true);
          return;
        }
        if (typeof window === 'undefined' || typeof window.RTCPeerConnection !== 'function') {
          failCall('WebRTC is not supported in this browser');
          return;
        }
        setCallState('acquiring', 'Requesting media...');
        placeholder.style.display = 'block';
        placeholder.textContent = 'Requesting microphone/camera...';
        const res = await tryCaptureWithFallback(wantAudio, wantVideo);
        if (st.destroyed || st.mediaGen !== mGen) {
          if (res.stream) stopStream(res.stream);
          return;
        }
        st.localStream = res.stream;
        st.mediaReady = true;
        if (res.stream) {
          applyTrackToggles(res.stream, Boolean(st.props.muted), Boolean(st.props.cameraOff));
        }
        setMediaState(res.mode, res.notice);
        applyLocalPreview();
        ensurePeerConnection();
        st.lastPropScreenShare = Boolean(st.props.screenShare);
        if (st.lastPropScreenShare) syncScreenShare(true);
      }

      async function retryCapture() {
        if (st.destroyed || !st.props.active || st.retrying) return;
        const wantAudio = st.props.audio !== false;
        const wantVideo = st.props.video !== false;
        const hasAudio = getLiveTracks(st.localStream, 'audio').length > 0;
        const hasVideo = getLiveTracks(st.localStream, 'video').length > 0;
        const needAudio = wantAudio && !hasAudio;
        const needVideo = wantVideo && !hasVideo;
        if (!needAudio && !needVideo) return;

        st.retrying = true;
        st.retryGen += 1;
        const mGen = st.mediaGen;
        const rGen = st.retryGen;
        retryBtn.disabled = true;
        retryBtn.textContent = 'Retrying...';
        try {
          const res = await tryCaptureWithFallback(needAudio, needVideo);
          if (st.destroyed || st.mediaGen !== mGen || st.retryGen !== rGen || !st.props.active) {
            if (res.stream) stopStream(res.stream);
            return;
          }
          const addedTracks = [];
          if (res.stream && typeof res.stream.getTracks === 'function') {
            const newTracks = res.stream.getTracks();
            if (!st.localStream) {
              st.localStream = res.stream;
              for (let i = 0; i < newTracks.length; i++) addedTracks.push(newTracks[i]);
            } else {
              for (let i = 0; i < newTracks.length; i++) {
                const nt = newTracks[i];
                const alreadyLive = getLiveTracks(st.localStream, nt.kind).length > 0;
                if (alreadyLive) {
                  try { nt.stop(); } catch (e) { /* ignore */ }
                } else {
                  if (typeof st.localStream.getTracks === 'function' && typeof st.localStream.removeTrack === 'function') {
                    const existing = st.localStream.getTracks();
                    for (let j = 0; j < existing.length; j++) {
                      if (existing[j] && existing[j].kind === nt.kind && existing[j].readyState === 'ended') {
                        try { st.localStream.removeTrack(existing[j]); } catch (e) { /* ignore */ }
                      }
                    }
                  }
                  if (typeof st.localStream.addTrack === 'function') {
                    try { st.localStream.addTrack(nt); } catch (e) { /* ignore */ }
                  }
                  addedTracks.push(nt);
                }
              }
            }
          }
          st.mediaReady = true;
          const finalHasAudio = getLiveTracks(st.localStream, 'audio').length > 0;
          const finalHasVideo = getLiveTracks(st.localStream, 'video').length > 0;
          const finalMode = finalHasAudio && finalHasVideo
            ? 'audio-video'
            : finalHasAudio
              ? 'audio-only'
              : finalHasVideo
                ? 'video-only'
                : 'recv-only';
          const stillMissing = (wantAudio && !finalHasAudio) || (wantVideo && !finalHasVideo);
          let finalNotice = '';
          if (stillMissing) {
            if (addedTracks.length === 0 && finalMode !== 'recv-only') {
              finalNotice = st.mediaNotice || (finalMode === 'audio-only'
                ? 'Camera unavailable — joined audio-only (can still view video & share screen)'
                : 'Microphone unavailable — joined video-only (can still listen & share screen)');
            } else {
              finalNotice = res.notice || st.mediaNotice;
            }
          }
          if (st.localStream) {
            applyTrackToggles(st.localStream, Boolean(st.props.muted), Boolean(st.props.cameraOff));
          }
          if (st.pc && addedTracks.length > 0) {
            let needNegotiate = false;
            for (let i = 0; i < addedTracks.length; i++) {
              const t = addedTracks[i];
              if (t.kind === 'video' && st.screenActive) continue;
              const tr = findTransceiverByKind(st.pc, t.kind);
              if (tr && tr.sender && typeof tr.sender.replaceTrack === 'function') {
                if (tr.direction !== 'sendrecv' && tr.direction !== 'sendonly') {
                  try { tr.direction = 'sendrecv'; } catch (e) { /* ignore */ }
                  needNegotiate = true;
                }
                try {
                  const p = tr.sender.replaceTrack(t);
                  if (p && typeof p.catch === 'function') p.catch(function () {});
                } catch (e) { /* ignore */ }
              } else {
                st.pc.addTrack(t, st.localStream || res.stream);
                needNegotiate = true;
              }
            }
            if (needNegotiate) renegotiate(st.peerGen);
          }
          setMediaState(finalMode, finalNotice);
          applyLocalPreview();
        } finally {
          if (!st.destroyed && st.retryGen === rGen) {
            st.retrying = false;
            retryBtn.disabled = false;
            retryBtn.textContent = 'Retry Cam/Mic';
            renderOverlayControls();
          }
        }
      }

      shareBtn.addEventListener('click', function () {
        if (st.destroyed || !st.props.active) return;
        if (st.screenWanted || st.screenActive) {
          st.screenWanted = false;
          stopScreenShare(true);
        } else {
          st.screenWanted = true;
          renderOverlayControls();
          startScreenShare();
        }
      });

      retryBtn.addEventListener('click', function () {
        retryCapture();
      });

      unmuteBtn.addEventListener('click', function () {
        if (st.destroyed) return;
        st.autoplayMuted = false;
        remoteVideo.muted = false;
        renderOverlayControls();
        playRemote();
      });

      function sync(nextProps) {
        if (st.destroyed) return;
        st.props = Object.assign({}, nextProps || {});
        applyHeight(st.props);
        const active = Boolean(st.props.active);
        st.iceServers = sanitizeIceServers(st.props.iceServers);

        if (!active) {
          if (st.pc || st.localStream || st.mediaKey || st.mediaReady || st.screenStream || st.screenWanted) {
            teardownAll(true);
          }
          return;
        }

        const wantAudio = st.props.audio !== false;
        const wantVideo = st.props.video !== false;
        const nextMediaKey = (wantAudio ? 'a' : '-') + (wantVideo ? 'v' : '-');

        if (!st.mediaKey || st.mediaKey !== nextMediaKey) {
          teardownAll(false);
          st.mediaKey = nextMediaKey;
          acquireMedia(st.mediaGen, wantAudio, wantVideo);
          return;
        }

        if (st.mediaReady) {
          if (st.localStream) {
            applyTrackToggles(st.localStream, Boolean(st.props.muted), Boolean(st.props.cameraOff));
          }
          renderOverlayControls();
          ensurePeerConnection();
          const wantScreen = Boolean(st.props.screenShare);
          if (wantScreen !== st.lastPropScreenShare) {
            st.lastPropScreenShare = wantScreen;
            syncScreenShare(wantScreen);
          }
        }
      }

      root.__tjVideoCall = { sync: sync, teardown: teardownAll, state: st };

      Promise.resolve().then(function () {
        if (!st.destroyed) sync(st.props);
      });

      return root;
    },

    update: function (el, props) {
      if (el && el.__tjVideoCall) el.__tjVideoCall.sync(props);
    },

    destroy: function (el) {
      if (el && el.__tjVideoCall) {
        el.__tjVideoCall.state.destroyed = true;
        el.__tjVideoCall.teardown(false);
        el.__tjVideoCall = null;
      }
    },
  };
}

/**
 * Returns `{ VideoCall: spec }` ready for `TinyJump.start({ components })`.
 */
export function createVideoCallComponents() {
  return { VideoCall: createVideoCallComponent() };
}

/**
 * Registers `<Native name="VideoCall" ... />` on a `registerComponent(name, spec)` function.
 */
export function registerVideoCall(registerComponentFn) {
  registerComponentFn('VideoCall', createVideoCallComponent());
}
