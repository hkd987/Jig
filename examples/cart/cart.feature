Feature: Shopping cart
  A cart adds up its items, and takes 10% off totals of 1000 cents or more.

  Scenario: Items add up
    Given an empty cart
    When I add 2 "apple" at 50 cents
    And I add 1 "bread" at 250 cents
    Then the total is 350 cents
    And the cart holds 3 items

  Scenario: Removing an item
    Given an empty cart
    When I add 3 "apple" at 50 cents
    And I remove "apple"
    Then the total is 0 cents

  Scenario Outline: Big orders get the discount
    Given an empty cart
    When I add <count> "widget" at 300 cents
    Then the total is <total> cents

    Examples:
      | count | total |
      | 3     | 900   |
      | 4     | 1080  |
      | 10    | 2700  |
