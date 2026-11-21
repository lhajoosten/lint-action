class Example:
    async def sample(self, items):
        nested = lambda x: x if x else 0
        try:
            match items:
                case [first, *_] if first:
                    return [nested(x) for x in items if x and x > 0]
                case _:
                    return []
        except ValueError:
            return []
