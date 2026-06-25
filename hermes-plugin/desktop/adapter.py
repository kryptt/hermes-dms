"""Desktop platform adapter for Hermes — bridges to the hermes-dms daemon.

Dials OUT (WebSocket client) to the daemon's ``/gateway`` endpoint on rh-anine.
Inbound desktop messages arrive as ``inbound`` frames and become gateway
``MessageEvent``s — so they run the full pipeline (slash commands like
``/model``, per-session model overrides, shared memory). Assistant output is
streamed back as ``draft`` frames (growing full text) and a final ``send``.

Mirrors ``plugins/platforms/irc/adapter.py`` (stdlib + bundled aiohttp; the
adapter dials out and relays). No new dependency — aiohttp ships with Hermes.
"""

from __future__ import annotations

import asyncio
import datetime
import json
import logging
import os
import time
from typing import Any, Dict, Optional

try:
    import aiohttp

    AIOHTTP_AVAILABLE = True
except ImportError:
    AIOHTTP_AVAILABLE = False
    aiohttp = None  # type: ignore[assignment]

from gateway.config import Platform
from gateway.platforms.base import (
    BasePlatformAdapter,
    MessageEvent,
    MessageType,
    SendResult,
)

logger = logging.getLogger(__name__)

DEFAULT_URL = "ws://10.20.0.3:9721/gateway"


class DesktopAdapter(BasePlatformAdapter):
    """Bridges the desktop panel (via hermes-dms) into the Hermes gateway."""

    def __init__(self, config, **kwargs):
        super().__init__(config=config, platform=Platform("desktop"))
        extra = getattr(config, "extra", {}) or {}
        self.url = os.getenv("DESKTOP_BRIDGE_URL") or extra.get("url", DEFAULT_URL)
        self.token = os.getenv("DESKTOP_BRIDGE_TOKEN") or extra.get("token", "")
        # The bridge WS is Bearer-authenticated and the daemon is a trusted,
        # network-isolated single-user component — so trust every sender on this
        # platform (no per-DM pairing). The gateway's authz reads this env via
        # the registry's allow_all_env (set in register()); seed it here so a
        # YAML-only setup needs no extra pod env. `extra.allow_all: false`
        # opts out.
        if extra.get("allow_all", True):
            os.environ.setdefault("DESKTOP_ALLOW_ALL_USERS", "true")
        # A home channel suppresses the per-new-conversation "📬 No home channel"
        # notice — which Hermes delivers as a `send`, masking the real first
        # reply. It's also where cron/cross-platform messages are delivered.
        home = os.getenv("DESKTOP_HOME_CHANNEL") or extra.get("home_channel", "desktop:home")
        os.environ.setdefault("DESKTOP_HOME_CHANNEL", home)
        self._session: "Optional[aiohttp.ClientSession]" = None
        self._ws = None
        self._supervisor: Optional[asyncio.Task] = None
        self._closing = False

    @property
    def name(self) -> str:
        return "Desktop"

    # ── Connection lifecycle ──────────────────────────────────────────────

    async def connect(self) -> bool:
        if not AIOHTTP_AVAILABLE:
            self._set_fatal_error(
                "aiohttp_missing", "aiohttp is required for the desktop adapter", retryable=False
            )
            return False
        if not self.url:
            self._set_fatal_error(
                "config_missing", "DESKTOP_BRIDGE_URL must be set", retryable=False
            )
            return False

        self._closing = False
        if self._session is None or self._session.closed:
            self._session = aiohttp.ClientSession()
        connected = await self._dial()
        if connected:
            self._mark_connected()
        # The supervisor owns the connection lifetime: it re-dials with backoff
        # whenever the socket drops (e.g. the daemon restarts), so we don't rely
        # on the gateway's connect-time-only reconnect watcher. Returning True
        # (even on a failed first dial) keeps that watcher from double-dialing.
        if self._supervisor is None or self._supervisor.done():
            self._supervisor = asyncio.create_task(self._supervise())
        return True

    async def disconnect(self) -> None:
        self._closing = True
        self._mark_disconnected()
        if self._supervisor and not self._supervisor.done():
            self._supervisor.cancel()
            try:
                await self._supervisor
            except asyncio.CancelledError:
                pass
        self._supervisor = None
        await self._close_socket()
        if self._session is not None and not self._session.closed:
            try:
                await self._session.close()
            except Exception:
                pass
        self._session = None

    async def _dial(self) -> bool:
        """One connection attempt. Returns True on success."""
        headers = {"Authorization": f"Bearer {self.token}"} if self.token else {}
        try:
            # heartbeat → WS ping frames detect a dead peer (e.g. daemon restart).
            self._ws = await self._session.ws_connect(self.url, headers=headers, heartbeat=30.0)
            logger.info("Desktop: connected to %s", self.url)
            return True
        except Exception as e:
            logger.warning("Desktop: connect to %s failed — %s", self.url, e)
            self._ws = None
            return False

    async def _close_socket(self) -> None:
        if self._ws is not None and not self._ws.closed:
            try:
                await self._ws.close()
            except Exception:
                pass
        self._ws = None

    async def _supervise(self) -> None:
        """Keep the bridge connection alive across daemon restarts."""
        backoff = 5
        try:
            while not self._closing:
                if self._ws is not None and not self._ws.closed:
                    await self._pump()  # blocks until the socket drops
                    if not self._closing:
                        self._mark_disconnected()
                if self._closing:
                    break
                if await self._dial():
                    self._mark_connected()
                    backoff = 5
                else:
                    await asyncio.sleep(backoff)
                    backoff = min(backoff * 2, 60)
        except asyncio.CancelledError:
            raise

    async def _pump(self) -> None:
        try:
            async for msg in self._ws:
                if msg.type == aiohttp.WSMsgType.TEXT:
                    await self._handle_frame(msg.data)
                elif msg.type in (
                    aiohttp.WSMsgType.CLOSE,
                    aiohttp.WSMsgType.CLOSED,
                    aiohttp.WSMsgType.ERROR,
                ):
                    break
        except asyncio.CancelledError:
            raise
        except Exception as e:
            logger.warning("Desktop: receive error — %s", e)
        await self._close_socket()

    async def _handle_frame(self, data: str) -> None:
        try:
            frame = json.loads(data)
        except (ValueError, TypeError):
            logger.warning("Desktop: ignoring malformed frame")
            return
        if frame.get("type") != "inbound":
            return
        chat_id = frame.get("chat_id") or "desktop:main"
        text = frame.get("text") or ""
        message_id = frame.get("message_id") or str(int(time.time() * 1000))
        source = self.build_source(
            chat_id=chat_id,
            chat_name=chat_id,
            chat_type="dm",
            user_id="desktop",
            user_name="desktop",
        )
        event = MessageEvent(
            text=text,
            message_type=MessageType.TEXT,
            source=source,
            message_id=message_id,
            timestamp=datetime.datetime.now(),
        )
        await self.handle_message(event)

    # ── Sending ───────────────────────────────────────────────────────────

    async def _send_frame(self, obj: Dict[str, Any]) -> bool:
        if self._ws is None or self._ws.closed:
            return False
        try:
            await self._ws.send_str(json.dumps(obj))
            return True
        except Exception as e:
            logger.warning("Desktop: send failed — %s", e)
            return False

    async def send(
        self,
        chat_id: str,
        content: str,
        reply_to: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
    ) -> SendResult:
        ok = await self._send_frame({"type": "send", "chat_id": chat_id, "text": content})
        return SendResult(success=ok, error=None if ok else "desktop bridge not connected")

    async def send_typing(self, chat_id: str, metadata: Optional[Dict[str, Any]] = None) -> None:
        await self._send_frame({"type": "typing", "chat_id": chat_id})

    def supports_draft_streaming(self, chat_type: Optional[str] = None) -> bool:
        return True

    async def send_draft(
        self,
        chat_id: str,
        draft_id: int,
        content: str,
        metadata: Optional[Dict[str, Any]] = None,
    ) -> SendResult:
        # Growing full text — the panel replaces the streaming bubble with it.
        ok = await self._send_frame({"type": "draft", "chat_id": chat_id, "text": content})
        return SendResult(success=ok, error=None if ok else "desktop bridge not connected")

    async def get_chat_info(self, chat_id: str) -> Dict[str, Any]:
        return {"name": chat_id, "type": "dm", "chat_id": chat_id}


def check_requirements() -> bool:
    """Dependency check (gateway platform_registry). The only hard requirement
    is aiohttp; config validity (URL/token) is handled by validate_config, so a
    YAML-only setup (no env vars) still loads."""
    return AIOHTTP_AVAILABLE


def validate_config(config) -> bool:
    """Gateway-runtime check — accepts config.yaml-only setup (extra.url)."""
    if not AIOHTTP_AVAILABLE:
        return False
    extra = getattr(config, "extra", {}) or {}
    return bool(os.getenv("DESKTOP_BRIDGE_URL") or extra.get("url", DEFAULT_URL))


def register(ctx):
    """Plugin entry point: called by the Hermes plugin system."""
    ctx.register_platform(
        name="desktop",
        label="Desktop",
        adapter_factory=lambda cfg: DesktopAdapter(cfg),
        check_fn=check_requirements,
        validate_config=validate_config,
        required_env=["DESKTOP_BRIDGE_URL"],
        # Trusted local bridge: the gateway honors this env (seeded in __init__)
        # to skip per-DM pairing. See _is_user_authorized's plugin-platform path.
        allow_all_env="DESKTOP_ALLOW_ALL_USERS",
        cron_deliver_env_var="DESKTOP_HOME_CHANNEL",
        install_hint="Requires aiohttp (bundled with Hermes)",
        # No platform line limit — the desktop panel renders long text fine, and
        # this avoids chunking a long reply into multiple `send` (→ multiple
        # chat_complete) frames.
        max_message_length=100000,
        emoji="🖥️",
        pii_safe=False,
        platform_hint=(
            "You are talking to the user through their Linux desktop "
            "(Niri / DankMaterialShell) via a floating panel. Markdown renders. "
            "This is the same user as on your other surfaces — share identity "
            "and memory."
        ),
    )
