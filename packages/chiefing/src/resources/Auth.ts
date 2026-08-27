import { AgentTokenManager } from '@/resources/AgentToken'
import type { HttpTransport } from '@/types/Transport'

/** The `ChiefdClient.auth` resource field: a thin wrapper binding the token
 * manager to this client's transport, matching every other resource client's
 * shape. Person enrolment is owned by the Rust host when it creates an agent
 * home; this client keeps only the token operation that panes use. */
export class AuthClient {
  constructor(private readonly transport: HttpTransport) {}

  tokenManager(identityId: string, privatePkcs8Pem: string): AgentTokenManager {
    return new AgentTokenManager(this.transport, identityId, privatePkcs8Pem)
  }
}
