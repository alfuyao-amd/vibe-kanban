import { useQuery } from '@tanstack/react-query';
import { useState, useCallback, useEffect, useMemo, useRef } from 'react';
import { sessionsApi } from '@/shared/lib/api';
import { useHostId } from '@/shared/providers/HostIdProvider';
import { workspaceSessionKeys } from '@/shared/hooks/workspaceSessionKeys';
import type { Session } from 'shared/types';

interface UseWorkspaceSessionsOptions {
  enabled?: boolean;
  /**
   * Pre-select this session id when the workspace first loads (or when
   * switching workspaces). Used by deep-links like
   * `/workspaces/<id>?session=<sid>` so callers can land on a specific
   * session rather than the most-recently-used one. If the id isn't in the
   * fetched sessions list, falls back to first-session selection.
   */
  initialSessionId?: string | null;
  /**
   * When set to a non-empty array, the returned `sessions` array is filtered
   * down to just these ids. Used by the embedded lead-agent chat to hide
   * worker / unrelated sessions from the conversation list. `null` /
   * `undefined` / empty array all mean "no filter — show every session".
   */
  sessionIdsAllow?: string[] | null;
}

/** Discriminated union for session selection state */
export type SessionSelection =
  | { mode: 'existing'; sessionId: string }
  | { mode: 'new' };

interface UseWorkspaceSessionsResult {
  sessions: Session[];
  selectedSession: Session | undefined;
  selectedSessionId: string | undefined;
  selectSession: (sessionId: string) => void;
  selectLatestSession: () => void;
  isLoading: boolean;
  /** Whether user is creating a new session */
  isNewSessionMode: boolean;
  /** Enter new session mode */
  startNewSession: () => void;
}

/**
 * Hook for managing sessions within a workspace.
 * Fetches all sessions for a workspace and provides session switching capability.
 * Sessions are ordered by most recently used (latest non-dev server execution first).
 */
export function useWorkspaceSessions(
  workspaceId: string | undefined,
  options: UseWorkspaceSessionsOptions = {}
): UseWorkspaceSessionsResult {
  const hostId = useHostId();
  const {
    enabled = true,
    initialSessionId = null,
    sessionIdsAllow = null,
  } = options;
  const [selection, setSelection] = useState<SessionSelection | undefined>(
    undefined
  );
  const prevWorkspaceIdRef = useRef(workspaceId);
  // One-shot consumption: once an initialSessionId has been honoured for a
  // given workspace mount, ignore further changes (e.g. user clicking a
  // different session in the list) so we don't keep snapping back.
  const consumedInitialRef = useRef(false);

  const { data: rawSessions = [], isLoading } = useQuery<Session[]>({
    queryKey: workspaceSessionKeys.byWorkspace(workspaceId, hostId),
    queryFn: () => sessionsApi.getByWorkspace(workspaceId!),
    enabled: enabled && !!workspaceId,
  });

  // Optional allow-list filter. The lead-agent embedded chat passes just the
  // lead session's id so the conversation list doesn't surface workers or
  // unrelated user sessions. Memoised so referential identity stays stable
  // across renders when the filter doesn't change.
  const sessions = useMemo(() => {
    if (!sessionIdsAllow || sessionIdsAllow.length === 0) return rawSessions;
    const allow = new Set(sessionIdsAllow);
    return rawSessions.filter((s) => allow.has(s.id));
  }, [rawSessions, sessionIdsAllow]);

  // Combined effect: handle workspace changes and auto-select sessions
  // This replaces two separate effects that had a race condition where the reset
  // effect would fire after auto-select when sessions were cached, undoing the selection.
  useEffect(() => {
    const workspaceChanged = prevWorkspaceIdRef.current !== workspaceId;
    prevWorkspaceIdRef.current = workspaceId;
    if (workspaceChanged) {
      consumedInitialRef.current = false;
    }

    if (sessions.length > 0) {
      // Sessions are ordered by most recently used, so first is the most recently used.
      // Honour `initialSessionId` (deep-link) the first time it lands in a
      // matching session list; afterwards leave selection alone so user
      // interaction wins.
      setSelection((prev) => {
        if (prev?.mode === 'new' && !workspaceChanged) return prev;
        if (
          !consumedInitialRef.current &&
          initialSessionId &&
          sessions.some((s) => s.id === initialSessionId)
        ) {
          consumedInitialRef.current = true;
          return { mode: 'existing', sessionId: initialSessionId };
        }
        if (prev?.mode === 'existing' && !workspaceChanged) return prev;
        return { mode: 'existing', sessionId: sessions[0].id };
      });
    } else {
      setSelection(undefined);
    }
  }, [workspaceId, sessions, initialSessionId]);

  const isNewSessionMode = selection?.mode === 'new' || sessions.length === 0;
  const selectedSessionId =
    selection?.mode === 'existing' ? selection.sessionId : undefined;

  const selectedSession = useMemo(
    () => sessions.find((s) => s.id === selectedSessionId),
    [sessions, selectedSessionId]
  );

  const selectSession = useCallback((sessionId: string) => {
    setSelection({ mode: 'existing', sessionId });
  }, []);

  const selectLatestSession = useCallback(() => {
    if (sessions.length > 0) {
      setSelection({ mode: 'existing', sessionId: sessions[0].id });
    }
  }, [sessions]);

  const startNewSession = useCallback(() => {
    setSelection({ mode: 'new' });
  }, []);

  return {
    sessions,
    selectedSession,
    selectedSessionId,
    selectSession,
    selectLatestSession,
    isLoading,
    isNewSessionMode,
    startNewSession,
  };
}
