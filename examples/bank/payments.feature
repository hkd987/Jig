Feature: Payments
  Money moves between accounts, and the bank never creates or loses any.

  Scenario: A payment moves money
    Given an account "ann" with 100 cents
    And an account "bob" with 20 cents
    When "ann" pays 30 cents to "bob"
    Then "ann" has 70 cents
    And "bob" has 50 cents
    And the bank holds 120 cents in total

  Scenario: An overdraft is refused
    Given an account "ann" with 10 cents
    And an account "bob" with 0 cents
    When "ann" pays 11 cents to "bob"
    Then the payment is refused
    And "ann" has 10 cents

  Scenario Outline: Payments never change the total
    Given an account "ann" with <start> cents
    And an account "bob" with 5 cents
    When "ann" pays <amount> cents to "bob"
    Then the bank holds <total> cents in total

    Examples:
      | start | amount | total |
      | 100   | 30     | 105   |
      | 10    | 50     | 15    |
      | 0     | 0      | 5     |
