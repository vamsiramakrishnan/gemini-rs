"""Gemini agents on the gemini-rs Rust runtime.

One compiled module (``gemini_adk._gemini_adk``, built by maturin) and this
thin layer: the ``@tool`` decorator, which turns a Python function into a
runtime tool with a JSON Schema derived from its type hints, and ``ask_as``,
which returns a typed answer as a Pydantic model (``gemini-adk[pydantic]``).

Example::

    import gemini_adk as adk

    @adk.tool
    def get_weather(city: str) -> dict:
        \"\"\"Get the current weather for a city.\"\"\"
        return {"city": city, "condition": "rain", "celsius": 14}

    agent = adk.Agent("weather", instruction="Use your tool.", tools=[get_weather])
    print(agent.ask("Do I need an umbrella in Paris?"))

Synchronous calls release the GIL while the Rust runtime works. The
``*_async`` forms and ``async for`` over streams and Live events integrate
with asyncio. Python code never runs on the Live session's fast lane; see
``docs/cleanup/python-binding-design.md`` in the repository.
"""

from __future__ import annotations

import inspect
import types
import typing
from typing import Any, Callable, TypeVar, overload

from ._gemini_adk import (
    Agent,
    Chat,
    ConfigError,
    Conversation,
    EventStream,
    GeminiAdkError,
    InvalidOutputError,
    LiveEvents,
    LiveSession,
    LlmError,
    MockLlm,
    SessionClosedError,
    SessionError,
    SessionSpec,
    Sim,
    SpecError,
    StateError,
    TimeoutError,
    Tool,
    ToolArgumentError,
    ToolError,
    ToolExecutionError,
    ToolNotFoundError,
    TransferError,
    __version__,
    connect,
    conversation_schema,
    scripted,
    validate_conversation,
)

__all__ = [
    "Agent",
    "Chat",
    "ConfigError",
    "Conversation",
    "EventStream",
    "GeminiAdkError",
    "InvalidOutputError",
    "LiveEvents",
    "LiveSession",
    "LlmError",
    "MockLlm",
    "SessionClosedError",
    "SessionError",
    "SessionSpec",
    "Sim",
    "SpecError",
    "StateError",
    "TimeoutError",
    "Tool",
    "ToolArgumentError",
    "ToolError",
    "ToolExecutionError",
    "ToolNotFoundError",
    "TransferError",
    "__version__",
    "ask_as",
    "connect",
    "conversation_schema",
    "schema_for",
    "scripted",
    "tool",
    "validate_conversation",
]

_F = TypeVar("_F", bound=Callable[..., Any])

_SCALARS: dict[Any, dict[str, Any]] = {
    str: {"type": "string"},
    int: {"type": "integer"},
    float: {"type": "number"},
    bool: {"type": "boolean"},
    type(None): {"type": "null"},
    Any: {},
}


def _schema_of(annotation: Any) -> dict[str, Any]:
    """The JSON Schema for one type hint. Unknown types become ``{}``."""
    if annotation is inspect.Parameter.empty:
        return {}
    if annotation in _SCALARS:
        return dict(_SCALARS[annotation])
    origin = typing.get_origin(annotation)
    args = typing.get_args(annotation)
    if origin is None:
        if annotation in (dict,):
            return {"type": "object"}
        if annotation in (list, tuple, set, frozenset):
            return {"type": "array"}
        if hasattr(annotation, "model_json_schema"):
            return typing.cast(dict[str, Any], annotation.model_json_schema())
        return {}
    if origin in (list, set, frozenset, tuple):
        items = _schema_of(args[0]) if args else {}
        return {"type": "array", "items": items}
    if origin is dict:
        return {"type": "object"}
    if origin is typing.Literal:
        return {"enum": list(args)}
    if origin is typing.Union or origin is types.UnionType:
        members = [_schema_of(a) for a in args]
        return members[0] if len(members) == 1 else {"anyOf": members}
    if origin is typing.Annotated:
        schema = _schema_of(args[0])
        for extra in args[1:]:
            if isinstance(extra, str):
                schema["description"] = extra
        return schema
    return {}


def _param_docs(doc: str) -> dict[str, str]:
    """``name: description`` lines from an ``Args:``/``Arguments:`` section."""
    docs: dict[str, str] = {}
    in_args = False
    for raw in doc.splitlines():
        line = raw.strip()
        if line.lower().rstrip(":") in ("args", "arguments", "parameters"):
            in_args = True
            continue
        if in_args:
            if not line:
                break
            if ":" in line:
                name, _, text = line.partition(":")
                name = name.strip().split(" ")[0].strip("*`")
                docs[name] = text.strip()
    return docs


def schema_for(func: Callable[..., Any]) -> dict[str, Any]:
    """The JSON Schema of ``func``'s keyword arguments, from its type hints
    and the ``Args:`` section of its docstring."""
    hints = typing.get_type_hints(func, include_extras=True)
    docs = _param_docs(inspect.getdoc(func) or "")
    properties: dict[str, Any] = {}
    required: list[str] = []
    for name, param in inspect.signature(func).parameters.items():
        if param.kind in (param.VAR_POSITIONAL, param.VAR_KEYWORD):
            continue
        schema = _schema_of(hints.get(name, param.annotation))
        if name in docs and "description" not in schema:
            schema["description"] = docs[name]
        properties[name] = schema
        if param.default is inspect.Parameter.empty:
            required.append(name)
    out: dict[str, Any] = {"type": "object", "properties": properties}
    if required:
        out["required"] = required
    return out


@overload
def tool(func: _F, /) -> Tool: ...
@overload
def tool(*, name: str | None = None, description: str | None = None) -> Callable[[_F], Tool]: ...


def tool(func: Any = None, /, *, name: str | None = None, description: str | None = None) -> Any:
    """Make a function a runtime tool.

    The tool's name is the function's, its description the first paragraph
    of the docstring, and its parameters the JSON Schema of the signature
    (``schema_for``). ``async def`` functions run on the asyncio loop that
    is running when the tool is created; create them inside the coroutine
    that runs the agent. The returned ``Tool`` is still callable directly.
    """

    def wrap(f: Callable[..., Any]) -> Tool:
        doc = inspect.getdoc(f) or ""
        summary = doc.split("\n\n", 1)[0].strip() if doc else ""
        return Tool(
            f,
            name=name or f.__name__,
            description=description or summary or f.__name__,
            parameters=schema_for(f),
        )

    return wrap(func) if func is not None else wrap


_M = TypeVar("_M")


def ask_as(agent: Agent, prompt: str, model: type[_M]) -> _M:
    """Ask for a typed answer: ``model`` is a Pydantic model class; its JSON
    Schema is the response schema and the reply is validated into it.
    Needs ``gemini-adk[pydantic]``."""
    schema = getattr(model, "model_json_schema", None)
    validate = getattr(model, "model_validate", None)
    if schema is None or validate is None:
        raise TypeError("ask_as needs a pydantic.BaseModel subclass (pip install 'gemini-adk[pydantic]')")
    return typing.cast(_M, validate(agent.ask(prompt, schema=schema())))


async def ask_as_async(agent: Agent, prompt: str, model: type[_M]) -> _M:
    """``await``-able ``ask_as``."""
    schema = getattr(model, "model_json_schema", None)
    validate = getattr(model, "model_validate", None)
    if schema is None or validate is None:
        raise TypeError("ask_as_async needs a pydantic.BaseModel subclass (pip install 'gemini-adk[pydantic]')")
    return typing.cast(_M, validate(await agent.ask_async(prompt, schema=schema())))


__all__.append("ask_as_async")
