"""The quickstart examples (examples/quickstart/src/bin), ported to Python
and run against `MockLlm` so CI needs no credentials."""

import gemini_adk as adk


def test_hello_text():
    agent = adk.Agent("assistant", instruction="Answer in one sentence.", llm=adk.MockLlm(["A realtime voice API."]))
    assert agent.ask("What is the Gemini Live API?") == "A realtime voice API."


def test_tool():
    @adk.tool
    def get_weather(city: str) -> dict:
        """Get the current weather for a city.

        Args:
            city: The city name, e.g. "Paris".
        """
        return {"city": city, "condition": "rain", "celsius": 14}

    llm = adk.MockLlm([{"tool": "get_weather", "args": {"city": "Paris"}}, "Yes, take an umbrella."])
    agent = adk.Agent("weather", instruction="Answer weather questions using your tool.", tools=[get_weather], llm=llm)
    assert agent.ask("Do I need an umbrella in Paris?") == "Yes, take an umbrella."
    assert "rain" in str(llm.requests()[1])


def test_typed():
    llm = adk.MockLlm(['{"name": "Tokyo", "country": "Japan", "population_millions": 37.4}'])
    schema = {
        "type": "object",
        "properties": {"name": {"type": "string"}, "country": {"type": "string"}, "population_millions": {"type": "number"}},
        "required": ["name", "country", "population_millions"],
    }
    city = adk.Agent("geographer", llm=llm).ask("Describe the largest city in Japan.", schema=schema)
    assert f"{city['name']}, {city['country']}: {city['population_millions']:.1f} million people" == "Tokyo, Japan: 37.4 million people"


def test_chat_streaming():
    llm = adk.MockLlm(["Hello Ada.", "Your name is Ada."])
    chat = adk.Agent("assistant", instruction="Be brief.", llm=llm).chat()
    replies = []
    for message in ["Hi, I'm Ada.", "What's my name?"]:
        text = "".join(e["text"] for e in chat.send_stream(message) if e["kind"] == "text")
        replies.append(text)
    assert replies == ["Hello Ada.", "Your name is Ada."]
