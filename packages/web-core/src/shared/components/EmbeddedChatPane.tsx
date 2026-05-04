import { ExecutionProcessesProvider } from '@/shared/providers/ExecutionProcessesProvider';
import { ReviewProvider } from '@/shared/hooks/ReviewProvider';
import { useWorkspaceContext } from '@/shared/hooks/useWorkspaceContext';
import { WorkspacesMainContainer } from '@/pages/workspaces/WorkspacesMainContainer';

/**
 * Bare-minimum chat surface for embedding into non-workspace routes.
 *
 * Renders just the conversation list + composer that `WorkspacesLayout`
 * normally wraps in resizable panels alongside the workspace sidebar,
 * changes panel, preview, etc. The lead-agent route uses this so the
 * embedded chat stays "lean" — no workspaces sidebar (the user is in a
 * project context, not picking workspaces) and no git/diff panel
 * (procedure runs surface diffs in their own pages).
 *
 * Pulls all data from the surrounding `WorkspaceProvider`. Mount inside an
 * override-props `WorkspaceProvider` to pin the workspace + session.
 *
 * IMPORTANT: also remounts an `ExecutionProcessesProvider` keyed off the
 * inner provider's `selectedSessionId`. The app-root mount of that
 * provider reads from the OUTER (URL-driven) WorkspaceProvider, which has
 * no session on non-workspace routes, so chat messages would stream to
 * nowhere without this re-wrap.
 */
export function EmbeddedChatPane() {
  const {
    workspace: selectedWorkspace,
    isLoading,
    selectedSession,
    selectedSessionId,
    sessions,
    isSessionsLoading,
    selectSession,
    repos,
    isNewSessionMode,
    startNewSession,
  } = useWorkspaceContext();

  return (
    <ExecutionProcessesProvider sessionId={selectedSessionId}>
      <ReviewProvider workspaceId={selectedWorkspace?.id}>
        <div
          className="flex flex-col h-full min-h-0 vk-embedded-chat-concise"
          data-conversation-density="concise"
        >
          {/* Visual hierarchy for the embedded chat: agent's user-facing
              message stands out; the model's intermediate thinking and
              tool calls fade so the user's eye lands on the conversation
              first. Targets data-entry-kind from DisplayConversationEntry.
              Scoped to .vk-embedded-chat-concise so the regular workspace
              chat is unaffected. */}
          <style>{`
            .vk-embedded-chat-concise [data-entry-kind="thinking"] {
              opacity: 0.55;
              font-style: italic;
            }
            .vk-embedded-chat-concise [data-entry-kind="tool_use"] {
              opacity: 0.7;
            }
            .vk-embedded-chat-concise [data-entry-kind="assistant_message"] {
              border-left: 3px solid rgb(59 130 246 / 0.6);
              padding-left: 0.5rem;
              margin-left: -0.625rem;
            }
            .vk-embedded-chat-concise [data-entry-kind="assistant_message"] :where(p, li, h1, h2, h3) {
              font-weight: 500;
            }
          `}</style>
          <WorkspacesMainContainer
            selectedWorkspace={selectedWorkspace ?? null}
            selectedSession={selectedSession}
            selectedSessionId={selectedSessionId}
            sessions={sessions}
            repos={repos}
            onSelectSession={selectSession}
            isLoading={isLoading}
            isSessionsLoading={isSessionsLoading}
            isNewSessionMode={isNewSessionMode}
            onStartNewSession={startNewSession}
          />
        </div>
      </ReviewProvider>
    </ExecutionProcessesProvider>
  );
}
