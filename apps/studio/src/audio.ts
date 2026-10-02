import { useCallback, useEffect, useRef, useState, type RefObject } from 'react';

/** The audio engine already used by the web app, loaded from /static/js/audio.js. */
export interface AudioEngine {
  onAudioData: ((data: string) => void) | null;
  onPlaybackDrained: (() => void) | null;
  initPlayback(): Promise<void>;
  startRecording(): Promise<void>;
  stopRecording(): void;
  playAudio(data: string): void;
  playAudioBinary(data: ArrayBuffer): void;
  clearQueue(): void;
  destroy(): void;
}

declare const AudioManager: { new(): AudioEngine };
type MediaState = 'off' | 'starting' | 'on';
type SessionSocket = RefObject<{ ws: WebSocket; revision: number } | null>;

export function useSessionAudio(socket: SessionSocket, onError: (message: string) => void) {
  const engine = useRef<AudioEngine | null>(null);
  const enabled = useRef(false);
  const generation = useRef(0);
  const error = useRef(onError);
  error.current = onError;
  const [sound, setSound] = useState<MediaState>('off');
  const [microphone, setMicrophone] = useState<MediaState>('off');

  const notifyDrained = () => {
    const ws = socket.current?.ws;
    if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: 'playbackDrained' }));
  };
  const flush = () => {
    engine.current?.clearQueue();
    notifyDrained();
  };

  const stop = useCallback(() => {
    generation.current += 1;
    const current = engine.current;
    engine.current = null;
    enabled.current = false;
    current?.destroy();
    setSound('off');
    setMicrophone('off');
  }, []);

  useEffect(() => () => {
    generation.current += 1;
    const current = engine.current;
    engine.current = null;
    enabled.current = false;
    current?.destroy();
  }, []);

  const enableSound = async (): Promise<AudioEngine | null> => {
    let current = engine.current;
    try {
      if (!current) {
        if (typeof AudioManager === 'undefined') throw new Error('The shared audio engine did not load. Reload Studio.');
        current = new AudioManager();
        engine.current = current;
        const owner = current;
        current.onPlaybackDrained = () => {
          if (engine.current === owner && enabled.current) notifyDrained();
        };
      }
      if (enabled.current) return current;
      setSound('starting');
      await current.initPlayback();
      if (engine.current !== current) return null;
      enabled.current = true;
      setSound('on');
      return current;
    } catch (cause) {
      if (current && engine.current !== current) return null;
      stop();
      error.current(`Audio unavailable: ${cause instanceof Error ? cause.message : String(cause)}`);
      return null;
    }
  };

  const toggleSound = async () => {
    if (sound === 'starting') return;
    if (enabled.current) {
      enabled.current = false;
      flush();
      setSound('off');
    } else {
      await enableSound();
    }
  };

  const toggleMicrophone = async () => {
    if (microphone === 'starting') return;
    if (microphone === 'on') {
      if (engine.current) {
        engine.current.onAudioData = null;
        engine.current.stopRecording();
      }
      setMicrophone('off');
      return;
    }
    const ws = socket.current?.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    const started = generation.current;
    setMicrophone('starting');
    const current = await enableSound();
    if (generation.current !== started) return;
    if (!current || socket.current?.ws !== ws) {
      setMicrophone('off');
      return;
    }
    current.onAudioData = (data) => {
      if (engine.current === current && socket.current?.ws === ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'audio', data }));
      }
    };
    try {
      await current.startRecording();
      if (engine.current !== current || socket.current?.ws !== ws) {
        current.destroy();
        return;
      }
      flush();
      setMicrophone('on');
    } catch (cause) {
      current.stopRecording();
      if (engine.current !== current || socket.current?.ws !== ws) return;
      setMicrophone('off');
      error.current(`Microphone unavailable: ${cause instanceof Error ? cause.message : String(cause)}`);
    }
  };

  const playBinary = (data: ArrayBuffer) => {
    if (!enabled.current || !engine.current) return;
    try {
      engine.current.playAudioBinary(data);
    } catch {
      error.current('The session sent an invalid audio frame.');
    }
  };

  const receive = (data: unknown): boolean => {
    if (data instanceof ArrayBuffer) {
      playBinary(data);
      return true;
    }
    if (data instanceof Blob) {
      const owner = engine.current;
      const ws = socket.current?.ws;
      void data.arrayBuffer().then((buffer) => {
        if (engine.current === owner && socket.current?.ws === ws) playBinary(buffer);
      }).catch(() => {
        if (engine.current === owner && socket.current?.ws === ws) error.current('Could not read the session audio frame.');
      });
      return true;
    }
    return false;
  };

  return {
    sound, microphone, toggleSound, toggleMicrophone, receive, stop,
    interrupt: flush,
    turnComplete: () => { if (!enabled.current) notifyDrained(); },
    playBase64: (data: string) => {
      if (!enabled.current || !engine.current) return;
      try { engine.current.playAudio(data); } catch { error.current('The session sent invalid audio.'); }
    },
  };
}
