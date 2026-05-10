import { useState } from 'react';
import { create, useModal } from '@ebay/nice-modal-react';
import { Button } from '@vibe/ui/components/Button';
import { Input } from '@vibe/ui/components/Input';
import { Label } from '@vibe/ui/components/Label';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@vibe/ui/components/KeyboardDialog';
import { Alert, AlertDescription } from '@vibe/ui/components/Alert';
import { defineModal } from '@/shared/lib/modals';
import { projectsApi } from '@/shared/lib/api';
import type { Project } from 'shared/types';

export type CreateLocalProjectDialogProps = Record<string, never>;

export type CreateLocalProjectResult = {
  action: 'created' | 'canceled';
  project?: Project;
};

const CreateLocalProjectDialogImpl = create<CreateLocalProjectDialogProps>(
  () => {
    const modal = useModal();
    const [name, setName] = useState('');
    const [workingDir, setWorkingDir] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [isCreating, setIsCreating] = useState(false);

    const handleClose = (result: CreateLocalProjectResult) => {
      modal.resolve(result);
      modal.hide();
    };

    const handleSubmit = async (e: React.FormEvent) => {
      e.preventDefault();
      if (!name.trim() || isCreating) return;
      setIsCreating(true);
      setError(null);
      try {
        const project = await projectsApi.createLocal(
          name.trim(),
          workingDir.trim() || undefined
        );
        handleClose({ action: 'created', project });
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setIsCreating(false);
      }
    };

    return (
      <Dialog
        open={modal.visible}
        onOpenChange={(open) => {
          if (!open) handleClose({ action: 'canceled' });
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Create project</DialogTitle>
            <DialogDescription>
              A local project lives entirely on this machine — no org or cloud
              account required. You'll add repos to it as workspaces afterwards.
            </DialogDescription>
          </DialogHeader>
          <form onSubmit={handleSubmit} className="flex flex-col gap-3 py-2">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="project-name">Name</Label>
              <Input
                id="project-name"
                autoFocus
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="my-cool-project"
                disabled={isCreating}
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="project-working-dir">
                Default working dir{' '}
                <span className="text-low text-xs">(optional)</span>
              </Label>
              <Input
                id="project-working-dir"
                value={workingDir}
                onChange={(e) => setWorkingDir(e.target.value)}
                placeholder="/Users/you/code/my-cool-project"
                disabled={isCreating}
              />
              <span className="text-xs text-low">
                Sessions created under this project default their CWD here.
                Leave blank to decide per-workspace.
              </span>
            </div>
            {error && (
              <Alert variant="destructive">
                <AlertDescription>{error}</AlertDescription>
              </Alert>
            )}
            <DialogFooter className="mt-2">
              <Button
                type="button"
                variant="outline"
                onClick={() => handleClose({ action: 'canceled' })}
                disabled={isCreating}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={!name.trim() || isCreating}>
                {isCreating ? 'Creating…' : 'Create'}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    );
  }
);

export const CreateLocalProjectDialog = defineModal<
  CreateLocalProjectDialogProps,
  CreateLocalProjectResult
>(CreateLocalProjectDialogImpl);
